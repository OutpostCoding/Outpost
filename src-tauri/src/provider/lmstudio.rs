// ── surface: backend ──────────────────────────────────
// LM Studio / OpenAI-compatible backend (SSE streaming, tool-call assembly).
// Exports: LmStudioProvider · Related: provider/mod.rs, agent.rs

//! LM Studio backend. LM Studio serves an OpenAI-compatible API on
//! `http://localhost:1234/v1` by default: a JSON POST with `stream: true`,
//! parsing the Server-Sent-Events token stream into normalized events.
//!
//! Tool-calling translation (request mapping + streaming `tool_calls` assembly)
//! lives here per spec §2. It is wired now but gets its real workout in step 3,
//! once tools are actually offered to the model.

use async_stream::stream;
use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::{json, Value};

use super::engine_error::{classify_engine_error, unwrap_error_message, EngineErrorKind, CONTEXT_EXCEEDED_HINT};
use super::{ChatRequest, EventStream, Message, Provider, Role, StreamEvent, Tool, ToolCall, wire_dump};

/// Mid-stream idle timeout: once streaming has begun, if no bytes arrive for this
/// long, the upstream connection has stalled (e.g. inference node crashed or
/// silent TCP drop). Surface a clean error rather than hanging indefinitely.
const STREAM_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(75);

pub struct LmStudioProvider {
    base_url: String,
    api_key: Option<String>,
    http: reqwest::Client,
}

impl LmStudioProvider {
    /// `base_url` is the OpenAI-compatible root, e.g. `http://localhost:1234/v1`
    /// (LM Studio) or `https://api.mistral.ai/v1` (a hosted OpenAI-compatible API).
    /// `api_key`, when present, is sent as a bearer token — local LM Studio needs
    /// none; hosted providers do.
    pub fn new(base_url: impl Into<String>, api_key: Option<String>) -> Self {
        Self {
            base_url: base_url.into(),
            api_key,
            http: crate::provider::http_client(),
        }
    }

    fn execute_non_streaming(
        http: reqwest::Client,
        url: String,
        api_key: Option<String>,
        body: serde_json::Map<String, Value>,
        sequential: bool,
        reasoning_already_emitted: usize,
        delta_already_emitted: usize,
    ) -> EventStream {
        let s = stream! {
            let mut non_stream_map = body;
            non_stream_map.insert("stream".to_string(), Value::Bool(false));

            let mut req = http.post(&url).json(&Value::Object(non_stream_map));
            if let Some(key) = &api_key {
                req = req.bearer_auth(key);
            }

            match req.send().await {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        match resp.json::<Value>().await {
                            Ok(json) => {
                                let choice = &json["choices"][0];
                                let msg = &choice["message"];

                                let rcontent = msg
                                    .get("reasoning_content")
                                    .or_else(|| msg.get("reasoning"))
                                    .or_else(|| msg.get("reasoning_summary"))
                                    .or_else(|| msg.get("thought"))
                                    .and_then(Value::as_str)
                                    .unwrap_or("");

                                let full_content = msg
                                    .get("content")
                                    .and_then(Value::as_str)
                                    .unwrap_or("");

                                let (think_text, answer_text) =
                                    extract_reasoning_and_answer(rcontent, full_content);

                                if !think_text.is_empty() {
                                    let r_suffix = safe_unemitted_suffix(think_text, reasoning_already_emitted);
                                    if !r_suffix.is_empty() {
                                        yield StreamEvent::Reasoning { text: r_suffix.to_string() };
                                    }
                                }

                                if !answer_text.is_empty() {
                                    let c_suffix = safe_unemitted_suffix(answer_text, delta_already_emitted);
                                    if !c_suffix.is_empty() {
                                        yield StreamEvent::Delta { text: c_suffix.to_string() };
                                    }
                                }

                                let mut fallback_tools: Vec<ToolCall> = Vec::new();
                                if let Some(calls) = msg.get("tool_calls").and_then(Value::as_array) {
                                    for (i, tcd) in calls.iter().enumerate() {
                                        let id = tcd
                                            .get("id")
                                            .and_then(Value::as_str)
                                            .filter(|s| !s.is_empty())
                                            .map(String::from)
                                            .unwrap_or_else(|| format!("call_{}", i));
                                        let name = tcd
                                            .get("function")
                                            .and_then(|f| f.get("name"))
                                            .and_then(Value::as_str)
                                            .unwrap_or("")
                                            .to_string();
                                        let arguments = tcd
                                            .get("function")
                                            .and_then(|f| f.get("arguments"))
                                            .and_then(Value::as_str)
                                            .unwrap_or("{}")
                                            .to_string();
                                        let arguments = if arguments.trim().is_empty() {
                                            "{}".to_string()
                                        } else {
                                            arguments
                                        };
                                        fallback_tools.push(ToolCall { id, name, arguments });
                                    }
                                }

                                if sequential && fallback_tools.len() > 1 {
                                    eprintln!(
                                        "[lmstudio] non-streaming: truncating {} tool calls to 1 (sequential active)",
                                        fallback_tools.len()
                                    );
                                    fallback_tools.truncate(1);
                                }

                                let has_tools = !fallback_tools.is_empty();
                                for tc in fallback_tools {
                                    yield StreamEvent::ToolStart { name: tc.name.clone() };
                                    yield StreamEvent::ToolCall(tc);
                                }

                                let fallback_finish = match choice.get("finish_reason").and_then(Value::as_str) {
                                    Some("tool_calls") => Some("tool_calls".into()),
                                    Some(_) if has_tools => Some("tool_calls".into()),
                                    Some(other) => Some(other.into()),
                                    None if has_tools => Some("tool_calls".into()),
                                    None => None,
                                };

                                yield StreamEvent::Done { finish_reason: fallback_finish };
                            }
                            Err(e) => {
                                eprintln!("[lmstudio] non-streaming JSON parse error: {e}");
                                yield StreamEvent::Error {
                                    message: format!("LM Studio non-streaming JSON parse error: {e}"),
                                };
                            }
                        }
                    } else {
                        let err_body = resp.text().await.unwrap_or_default();
                        eprintln!("[lmstudio] non-streaming request failed with HTTP {status}: {err_body}");

                        match classify_engine_error(&err_body) {
                            EngineErrorKind::ContextExceeded(_) => {
                                eprintln!("[lmstudio] non-streaming: context length exceeded");
                                yield StreamEvent::Error {
                                    message: format!("LM Studio: {CONTEXT_EXCEEDED_HINT}"),
                                };
                                return;
                            }
                            EngineErrorKind::DiffParser(_) => {
                                let (extracted_text, mut salvaged) = salvage_tools_from_diff_error(&err_body);
                                if let Some(txt) = extracted_text {
                                    let s = safe_unemitted_suffix(&txt, delta_already_emitted);
                                    if !s.is_empty() {
                                        yield StreamEvent::Delta { text: s.to_string() };
                                    }
                                }
                                if !salvaged.is_empty() {
                                    if sequential && salvaged.len() > 1 {
                                        eprintln!(
                                            "[lmstudio] non-streaming: truncating {} salvaged tool calls to 1 (sequential active)",
                                            salvaged.len()
                                        );
                                        salvaged.truncate(1);
                                    }
                                    eprintln!(
                                        "[lmstudio] salvaged {} valid tool calls from non-streaming diff error: {:?}",
                                        salvaged.len(),
                                        salvaged.iter().map(|t| &t.name).collect::<Vec<_>>()
                                    );
                                    for tc in salvaged {
                                        yield StreamEvent::ToolStart { name: tc.name.clone() };
                                        yield StreamEvent::ToolCall(tc);
                                    }
                                    yield StreamEvent::Done { finish_reason: Some("tool_calls".into()) };
                                    return;
                                }
                            }
                            EngineErrorKind::Other(_) => {}
                        }

                        yield StreamEvent::Error {
                            message: format!("LM Studio returned HTTP {status}: {err_body}"),
                        };
                    }
                }
                Err(e) => {
                    eprintln!("[lmstudio] non-streaming network error: {e}");
                    yield StreamEvent::Error {
                        message: format!("LM Studio non-streaming network error: {e}"),
                    };
                }
            }
        };
        Box::pin(s)
    }
}

/// Per-index accumulator for streamed `tool_calls` fragments.
#[derive(Default)]
struct ToolCallAcc {
    id: String,
    name: String,
    arguments: String,
    /// Whether a ToolStart has been emitted for this call yet.
    started: bool,
}

/// A short label for error messages — the host[:port] from the base URL, so a
/// failure names the actual server ("openrouter.ai") rather than "LM Studio".
fn host_label(base_url: &str) -> &str {
    base_url
        .split("://")
        .nth(1)
        .unwrap_or(base_url)
        .split('/')
        .next()
        .unwrap_or(base_url)
}

fn role_str(role: &Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn to_openai_messages_with_opts(messages: &[Message], include_reasoning: bool) -> Vec<Value> {
    messages
        .iter()
        .map(|m| {
            let mut obj = serde_json::Map::new();
            obj.insert("role".into(), json!(role_str(&m.role)));
            let content = m.content.clone();
            // Vision: a user turn carrying image attachments becomes the OpenAI
            // multimodal content-parts array (a text part + one image_url per
            // image, base64 data URIs). Plain string otherwise (unchanged path).
            if matches!(m.role, Role::User) && !m.images.is_empty() {
                let mut parts: Vec<Value> = Vec::new();
                if !content.is_empty() {
                    parts.push(json!({ "type": "text", "text": content }));
                }
                for img in &m.images {
                    parts.push(json!({ "type": "image_url", "image_url": { "url": img } }));
                }
                obj.insert("content".into(), json!(parts));
            } else {
                obj.insert("content".into(), json!(content));
            }
            if matches!(m.role, Role::Assistant) && include_reasoning {
                // Replay reasoning for assistant turns when reasoning is present
                // (governed by expand_messages_with_segments for historical turns, and agent.rs for active turns).
                if let Some(r) = &m.reasoning {
                    if !r.trim().is_empty() {
                        obj.insert("reasoning_content".into(), json!(r));
                    }
                }
            }
            if !m.tool_calls.is_empty() {
                let calls: Vec<Value> = m
                    .tool_calls
                    .iter()
                    .map(|tc| {
                        json!({
                            "id": tc.id,
                            "type": "function",
                            "function": { "name": tc.name, "arguments": tc.arguments }
                        })
                    })
                    .collect();
                obj.insert("tool_calls".into(), json!(calls));
            }
            if let Some(id) = &m.tool_call_id {
                obj.insert("tool_call_id".into(), json!(id));
            }
            Value::Object(obj)
        })
        .collect()
}

#[allow(dead_code)]
pub(crate) fn to_openai_messages(messages: &[Message]) -> Vec<Value> {
    to_openai_messages_with_opts(messages, true)
}

/// The tool block exactly as it goes on the wire. `pub(crate)` so the context
/// inspector can measure THIS rather than its own approximation of it — the same
/// reason `preview_context` calls the live `tail_context`: a readout that builds
/// its own version of what it is reporting on will drift, and drift in a
/// measurement is worse than no measurement.
pub(crate) fn to_openai_tools(tools: &[Tool]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| {
            json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters
                }
            })
        })
        .collect()
}

/// Longest suffix of `s` that is a prefix of `marker`. Used to hold back a
/// trailing partial `<think>`/`</think>` tag that the next chunk may complete.
/// Markers are ASCII, so byte operations are boundary-safe.
fn longest_prefix_suffix(s: &str, marker: &str) -> usize {
    let max = marker.len().min(s.len());
    for k in (1..=max).rev() {
        if s.as_bytes().ends_with(&marker.as_bytes()[..k]) {
            return k;
        }
    }
    0
}

/// Returns the unemitted suffix of `full`, safely skipping up to `already_emitted` bytes
/// on a valid UTF-8 character boundary.
fn safe_unemitted_suffix(full: &str, already_emitted: usize) -> &str {
    if already_emitted == 0 {
        return full;
    }
    if already_emitted >= full.len() {
        return "";
    }
    let mut idx = already_emitted;
    while idx < full.len() && !full.is_char_boundary(idx) {
        idx += 1;
    }
    &full[idx..]
}

/// Separates reasoning content and answer text from non-streaming message fields.
/// If `rcontent` is present (from a dedicated field like `reasoning_content`), it is used directly.
/// Otherwise, `full_content` is parsed for `<think>...</think>` tags, or a leading reasoning block
/// terminated by `</think>` (common when the chat template prefills `<think>` in the prompt).
fn extract_reasoning_and_answer<'a>(rcontent: &'a str, full_content: &'a str) -> (&'a str, &'a str) {
    if !rcontent.is_empty() {
        (rcontent, full_content)
    } else if let Some(start) = full_content.find("<think>") {
        if full_content[..start].trim().is_empty() {
            if let Some(end) = full_content.find("</think>") {
                (&full_content[start + "<think>".len()..end], &full_content[end + "</think>".len()..])
            } else {
                (&full_content[start + "<think>".len()..], "")
            }
        } else {
            ("", full_content)
        }
    } else if let Some(end) = full_content.find("</think>") {
        // The prompt prefill or chat template included `<think>`, so the model's
        // generation began inside the reasoning block and emitted only `</think>` when finished.
        // Everything before `</think>` is reasoning; everything after is answer text.
        (&full_content[..end], &full_content[end + "</think>".len()..])
    } else {
        ("", full_content)
    }
}

/// Permissively extracts a complete, valid JSON object or array from the start of a string.
/// Discards trailing characters (e.g. extra closing braces, whitespace, or model stop tags
/// like `</tool_call>`) that cause standard strict `serde_json::from_str` to fail.
///
/// This is critical for LM Studio when a model emits extra trailing tokens after closing the
/// JSON payload (e.g. `{"path":"pyproject.toml"}}`), triggering:
/// "Engine protocol predict stream returned a tool call delta for already finalized tool call 0."
fn extract_valid_json_args(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Fast path: standard well-formed JSON object or array
    if let Ok(val) = serde_json::from_str::<Value>(trimmed) {
        if val.is_object() || val.is_array() {
            return Some(val.to_string());
        }
    }
    // Stream deserializer: extracts the first complete top-level JSON value,
    // ignoring any trailing tokens/junk.
    let mut de = serde_json::Deserializer::from_str(trimmed).into_iter::<Value>();
    if let Some(Ok(val)) = de.next() {
        if val.is_object() || val.is_array() {
            return Some(val.to_string());
        }
    }
    None
}

/// Extracts valid, completed tool calls and any accompanying text content from an
/// LM Studio / llama.cpp diff parser error message (e.g. "Invalid diff: now finding less tool calls!").
///
/// When a local model attempts multiple tool calls or interleaves commentary between calls,
/// llama.cpp's `common_chat_msg_diff` parser can throw a 500 error because the count of parsed calls
/// drops from token N to N+1. However, the error payload frequently includes the fully-formed
/// tool calls that were already validated (under `Previous (N):` or `Current (N):`), as well as
/// `Current msg text content:`. Salvaging these allows the turn to complete and execute the
/// intended tool rather than crashing the user's turn.
fn salvage_tools_from_diff_error(raw_err: &str) -> (Option<String>, Vec<ToolCall>) {
    let message_str = unwrap_error_message(raw_err);

    // Normalize escaped characters if the string arrived doubly-escaped
    let normalized = message_str
        .replace("\\n", "\n")
        .replace("\\\"", "\"")
        .replace("\\\\", "\\");

    let mut salvaged_tools: Vec<ToolCall> = Vec::new();

    // 2. Scan lines for tool call patterns: `- name: '...', args: '...'`
    for line in normalized.lines() {
        let trimmed_line = line.trim();
        if let Some(name_idx) = trimmed_line.find("- name: '") {
            let after_name = &trimmed_line[name_idx + "- name: '".len()..];
            if let Some(end_name) = after_name.find('\'') {
                let name = &after_name[..end_name];
                if let Some(args_idx) = trimmed_line.find(", args: '") {
                    let after_args = &trimmed_line[args_idx + ", args: '".len()..];
                    if let Some(end_args) = after_args.rfind('\'') {
                        let args_str = after_args[..end_args].trim();
                        // Validate that args_str contains a valid JSON object
                        if let Some(valid_args) = extract_valid_json_args(args_str) {
                            if !salvaged_tools
                                .iter()
                                .any(|t| t.name == name && t.arguments == valid_args)
                            {
                                salvaged_tools.push(ToolCall {
                                    id: format!("call_{}", salvaged_tools.len()),
                                    name: name.to_string(),
                                    arguments: valid_args,
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    // 3. Extract text content if present (under "Current msg text content:")
    let text_content = if let Some(pos) = normalized.find("Current msg text content:") {
        let raw_text = &normalized[pos + "Current msg text content:".len()..];
        // Strip out raw tool tags if present (e.g. `<tool_call>...`)
        let without_tag = if let Some(tag_pos) = raw_text.find("<tool_call>") {
            &raw_text[..tag_pos]
        } else if let Some(tag_pos) = raw_text.find("<tool_call") {
            &raw_text[..tag_pos]
        } else {
            raw_text
        };
        // Strip trailing JSON structure if sliced from an unparsed JSON response
        let without_json = if let Some(cut) = without_tag.find("\",\"type\"") {
            &without_tag[..cut]
        } else if let Some(cut) = without_tag.find("\"type\":") {
            &without_tag[..cut]
        } else {
            without_tag
        };
        let cleaned = without_json.trim();
        if !cleaned.is_empty() {
            Some(cleaned.to_string())
        } else {
            None
        }
    } else {
        None
    };

    (text_content, salvaged_tools)
}


/// Split streamed content around `<think>…</think>`: text outside the tags is
/// answer (`Delta`), text inside is reasoning (`Reasoning`). `carry` holds any
/// trailing partial tag between calls; `in_think` tracks whether we're mid-block.
///
/// `text_started` becomes true once any answer text has been emitted, and a think
/// block may only OPEN before then — at the leading edge of the answer. A `<think>`
/// that appears *after* answer text is the model typing the characters literally
/// (e.g. quoting a discussion of reasoning tags), so it's passed through as text, not
/// misread as a real reasoning block mid-sentence. Genuine inline-thinking models put
/// their `<think>…</think>` first, so they're unaffected.
fn push_split(
    out: &mut Vec<StreamEvent>,
    carry: &mut String,
    in_think: &mut bool,
    text_started: &mut bool,
) {
    loop {
        if *in_think {
            // Inside a reasoning block: consume until the closing tag.
            if let Some(pos) = carry.find("</think>") {
                let before = carry[..pos].to_string();
                if !before.is_empty() {
                    out.push(StreamEvent::Reasoning { text: before });
                }
                *carry = carry[pos + "</think>".len()..].to_string();
                *in_think = false;
            } else {
                let hold = longest_prefix_suffix(carry, "</think>");
                let emit_len = carry.len() - hold;
                if emit_len > 0 {
                    out.push(StreamEvent::Reasoning {
                        text: carry[..emit_len].to_string(),
                    });
                    *carry = carry[emit_len..].to_string();
                }
                break;
            }
        } else if *text_started {
            // Past the leading edge — everything is answer text; any `<think>` is literal.
            if !carry.is_empty() {
                out.push(StreamEvent::Delta { text: std::mem::take(carry) });
            }
            break;
        } else {
            // Leading edge: a `<think>` with only whitespace before it opens a real
            // reasoning block; anything else means the answer has started.
            match carry.find("<think>") {
                Some(pos) if carry[..pos].trim().is_empty() => {
                    *carry = carry[pos + "<think>".len()..].to_string();
                    *in_think = true;
                }
                Some(_) => {
                    // Real text precedes the tag → the answer started; emit it all as
                    // text (the `<think>` is literal).
                    *text_started = true;
                    out.push(StreamEvent::Delta { text: std::mem::take(carry) });
                    break;
                }
                None => {
                    // No `<think>` yet. If everything but a possible partial `<think>`
                    // tail is whitespace, we're still in the run-up — hold the tail.
                    // Otherwise the answer text has started.
                    let hold = longest_prefix_suffix(carry, "<think>");
                    let head_len = carry.len() - hold;
                    if carry[..head_len].trim().is_empty() {
                        if head_len > 0 {
                            out.push(StreamEvent::Delta {
                                text: carry[..head_len].to_string(),
                            });
                        }
                        *carry = carry[head_len..].to_string();
                    } else {
                        *text_started = true;
                        out.push(StreamEvent::Delta { text: std::mem::take(carry) });
                    }
                    break;
                }
            }
        }
    }
}

#[async_trait]
impl Provider for LmStudioProvider {
    async fn stream_chat(&self, request: ChatRequest) -> anyhow::Result<EventStream> {
        let url = format!("{}/chat/completions", self.base_url);
        // Whether any turn carries image attachments — used to give a plain-language
        // error if the provider's content schema is text-only (e.g. DeepSeek).
        let had_images = request.messages.iter().any(|m| !m.images.is_empty());

        // Resolve an empty model ("Auto") to whatever LM Studio is currently
        // serving — fetched fresh each turn, so it follows backend model switches.
        let model = if request.model.trim().is_empty() {
            self.list_models()
                .await?
                .into_iter()
                .next()
                .ok_or_else(|| anyhow::anyhow!("no model loaded (and none specified)"))?
        } else {
            request.model.clone()
        };

        // Build the body explicitly so unset optionals are omitted rather than
        // sent as `null` (stricter servers reject null).
        let mut body = serde_json::Map::new();
        body.insert("model".into(), json!(model));
        let include_reasoning = !self.base_url.contains("api.openai.com");
        let wire_messages = to_openai_messages_with_opts(&request.messages, include_reasoning);
        wire_dump(None, &wire_messages);
        body.insert("messages".into(), json!(wire_messages));
        body.insert("stream".into(), json!(true));
        if !request.tools.is_empty() {
            body.insert("tools".into(), json!(to_openai_tools(&request.tools)));
            body.insert("parallel_tool_calls".into(), json!(false));
        }
        if let Some(t) = request.temperature {
            body.insert("temperature".into(), json!(t));
        }
        if let Some(p) = request.top_p {
            body.insert("top_p".into(), json!(p));
        }
        if let Some(m) = request.max_tokens {
            body.insert("max_tokens".into(), json!(m));
        }
        // Reasoning control, per the connection's tagged dialect (only sent when a
        // dialect is set — a generic OpenAI server might reject these fields, and
        // local models handle thinking via `<think>` regardless). Folds the shared
        // 6-level Thinking control onto each provider's wire params.
        body.extend(crate::provider::reasoning_params(
            request.reasoning_style.as_deref(),
            request.thinking_effort.as_deref(),
        ));

        let sequential = request.sequential_tool_calls.unwrap_or(false);

        let mut req = self.http.post(&url).json(&Value::Object(body.clone()));
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }
        let resp = req.send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let detail = resp.text().await.unwrap_or_default();
            let trimmed = detail.trim();

            match classify_engine_error(trimmed) {
                EngineErrorKind::ContextExceeded(_) => {
                    eprintln!("[lmstudio] initial request: context length exceeded");
                    anyhow::bail!("LM Studio: {CONTEXT_EXCEEDED_HINT}");
                }
                EngineErrorKind::DiffParser(_) => {
                    eprintln!(
                        "[lmstudio] caught LM Studio streaming parser error on initial request ({status}: {trimmed})."
                    );

                    let (extracted_text, mut salvaged) = salvage_tools_from_diff_error(trimmed);
                    if !salvaged.is_empty() {
                        if sequential && salvaged.len() > 1 {
                            eprintln!(
                                "[lmstudio] truncating {} salvaged tool calls to 1 (sequential active)",
                                salvaged.len()
                            );
                            salvaged.truncate(1);
                        }
                        eprintln!(
                            "[lmstudio] salvaged {} valid tool calls directly from initial diff error: {:?}",
                            salvaged.len(),
                            salvaged.iter().map(|t| &t.name).collect::<Vec<_>>()
                        );
                        let s = stream! {
                            if let Some(txt) = extracted_text {
                                yield StreamEvent::Delta { text: txt };
                            }
                            for tc in salvaged {
                                yield StreamEvent::ToolStart { name: tc.name.clone() };
                                yield StreamEvent::ToolCall(tc);
                            }
                            yield StreamEvent::Done { finish_reason: Some("tool_calls".into()) };
                        };
                        return Ok(Box::pin(s));
                    }

                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                    return Ok(Self::execute_non_streaming(
                        self.http.clone(),
                        url,
                        self.api_key.clone(),
                        body,
                        sequential,
                        0,
                        0,
                    ));
                }
                EngineErrorKind::Other(_) => {}
            }

            // A provider whose content schema is text-only (e.g. DeepSeek) rejects
            // image parts with a cryptic 400 — give a plain-language hint instead.
            if had_images && trimmed.to_lowercase().contains("image") {
                anyhow::bail!(
                    "This model or provider doesn't accept images. Attach an image only when a vision model is selected — try a vision model in LM Studio, or a vision provider like OpenRouter, OpenAI, or Gemini."
                );
            }
            // LM Studio returns a full HTML error page when the model itself fails
            // (commonly a small model that can't handle tool-calling). Don't dump
            // the HTML at the user — give a concise, actionable message instead.
            let detail = if trimmed.is_empty()
                || trimmed.starts_with("<!DOCTYPE")
                || trimmed.starts_with("<html")
            {
                "the model returned an error with no details. Two common causes: (1) the conversation \
                 exceeded the model's loaded context window — likely on a long turn; raise the context \
                 length in LM Studio or start a fresh thread; (2) the model doesn't support tool-calling \
                 — turn tools off, or use a tool-capable model."
                    .to_string()
            } else {
                let short: String = trimmed.chars().take(500).collect();
                if trimmed.chars().count() > 500 {
                    format!("{short}…")
                } else {
                    short
                }
            };
            anyhow::bail!("{} returned {status}: {detail}", host_label(&self.base_url));
        }

        let fallback_url = url.clone();
        let fallback_http = self.http.clone();
        let fallback_api_key = self.api_key.clone();
        let fallback_body_map = body.clone();

        let mut bytes_stream = Some(resp.bytes_stream());
        let s = stream! {
            // Buffer raw bytes and decode whole lines only, so a multi-byte UTF-8
            // character split across network chunks isn't corrupted — matters for
            // large/non-ASCII tool arguments (e.g. an essay with smart quotes).
            let mut buf: Vec<u8> = Vec::new();
            let mut tools_acc: Vec<ToolCallAcc> = Vec::new();
            let mut finish_reason: Option<String> = None;
            // Reasoning state: `carry` holds pending content (and any partial think
            // tag); `in_think` tracks whether we're inside a <think> block;
            // `text_started` gates opening one to the leading edge of the answer.
            let mut carry = String::new();
            let mut in_think = false;
            let mut text_started = false;
            let mut reasoning_emitted_len: usize = 0;
            let mut delta_emitted_len: usize = 0;

            let mut started = false;
            'outer: loop {
                let next_chunk = if started {
                    match bytes_stream.as_mut() {
                        Some(bs) => match tokio::time::timeout(STREAM_IDLE_TIMEOUT, bs.next()).await {
                            Ok(chunk) => chunk,
                            Err(_) => {
                                yield StreamEvent::Error {
                                    message: format!(
                                        "Stream idle timeout: no data received from server for {}s mid-stream",
                                        STREAM_IDLE_TIMEOUT.as_secs()
                                    ),
                                };
                                return;
                            }
                        },
                        None => None,
                    }
                } else {
                    match bytes_stream.as_mut() {
                        Some(bs) => bs.next().await,
                        None => None,
                    }
                };

                let Some(chunk) = next_chunk else { break 'outer };
                started = true;
                let b = match chunk {
                    Ok(b) => b,
                    Err(e) => {
                        yield StreamEvent::Error { message: format!("stream error: {e}") };
                        return;
                    }
                };
                buf.extend_from_slice(&b);

                // SSE frames are newline-delimited; drain complete lines.
                while let Some(nl) = buf.iter().position(|&c| c == b'\n') {
                    let line_bytes: Vec<u8> = buf.drain(..=nl).collect();
                    let line = String::from_utf8_lossy(&line_bytes);
                    let line = line.trim();
                    let Some(data) = line.strip_prefix("data:") else { continue };
                    let data = data.trim();
                    if data == "[DONE]" {
                        break 'outer;
                    }
                    if data.is_empty() {
                        continue;
                    }
                    let Ok(v) = serde_json::from_str::<Value>(data) else {
                        continue; // ignore keep-alives / non-JSON lines
                    };

                    // An error can arrive mid-stream, as its own frame. It matches
                    // none of the `choices[0]` lookups below, so it used to be
                    // dropped — and if it landed AFTER some content, the turn was
                    // recorded and reported as a clean stop, showing the user a
                    // truncated answer as though it were the whole reply. The
                    // Anthropic adapter has always had this branch; this one
                    // serves every other provider, so it needed it more.
                    if let Some(err) = v.get("error") {
                        let message = err
                            .get("message")
                            .and_then(|m| m.as_str())
                            .unwrap_or("the provider reported an error mid-response");

                        match classify_engine_error(message) {
                            EngineErrorKind::ContextExceeded(_) => {
                                eprintln!("[lmstudio] mid-stream: context length exceeded");
                                yield StreamEvent::Error {
                                    message: format!("LM Studio: {CONTEXT_EXCEEDED_HINT}"),
                                };
                                return;
                            }
                            EngineErrorKind::DiffParser(_) => {
                                eprintln!(
                                    "[lmstudio] caught LM Studio streaming parser error mid-stream ({message})."
                                );
                                drop(bytes_stream.take());

                                // Check 1: Collect any valid completed tool calls already accumulated in tools_acc.
                                // Preserve whether ToolStart was already emitted during streaming to avoid duplicate ToolStart events.
                                let mut valid_tools: Vec<(ToolCall, bool)> = Vec::new();
                                for (i, t) in tools_acc.drain(..).enumerate() {
                                    if !t.name.is_empty() {
                                        if let Some(valid_args) = extract_valid_json_args(&t.arguments) {
                                            valid_tools.push((
                                                ToolCall {
                                                    id: if t.id.is_empty() { format!("call_{i}") } else { t.id },
                                                    name: t.name,
                                                    arguments: valid_args,
                                                },
                                                t.started,
                                            ));
                                        }
                                    }
                                }

                                // Check 2: If tools_acc had no valid completed tool call, salvage from error message
                                if valid_tools.is_empty() {
                                    let (extracted_text, salvaged) = salvage_tools_from_diff_error(message);
                                    if let Some(txt) = extracted_text {
                                        let s = safe_unemitted_suffix(&txt, delta_emitted_len);
                                        if !s.is_empty() {
                                            yield StreamEvent::Delta { text: s.to_string() };
                                        }
                                    }
                                    // Salvaged calls from the diff error payload have not emitted ToolStart yet
                                    valid_tools = salvaged.into_iter().map(|tc| (tc, false)).collect();
                                }

                                // If we have at least one valid tool call, finish the round successfully!
                                if !valid_tools.is_empty() {
                                    if sequential && valid_tools.len() > 1 {
                                        eprintln!(
                                            "[lmstudio] truncating {} salvaged tool calls to 1 (sequential active)",
                                            valid_tools.len()
                                        );
                                        valid_tools.truncate(1);
                                    }
                                    eprintln!(
                                        "[lmstudio] salvaged {} valid tool calls from diff parser error: {:?}",
                                        valid_tools.len(),
                                        valid_tools.iter().map(|(t, _)| &t.name).collect::<Vec<_>>()
                                    );
                                    for (tc, already_started) in valid_tools {
                                        if !already_started {
                                            yield StreamEvent::ToolStart { name: tc.name.clone() };
                                        }
                                        yield StreamEvent::ToolCall(tc);
                                    }
                                    yield StreamEvent::Done { finish_reason: Some("tool_calls".into()) };
                                    return;
                                }

                                // Fallback to non-streaming as a best-effort last resort for diff errors only
                                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                                let mut fallback_stream = Self::execute_non_streaming(
                                    fallback_http,
                                    fallback_url,
                                    fallback_api_key,
                                    fallback_body_map,
                                    sequential,
                                    reasoning_emitted_len,
                                    delta_emitted_len,
                                );

                                while let Some(ev) = fallback_stream.next().await {
                                    yield ev;
                                }
                                return;
                            }
                            EngineErrorKind::Other(_) => {
                                yield StreamEvent::Error { message: message.to_string() };
                                break 'outer;
                            }
                        }
                    }

                    // Some backends stream thinking in a dedicated field
                    // (`reasoning_content` / `reasoning` / `reasoning_summary` / `thought`);
                    // others inline it as <think>…</think> inside `content`. Handle all shapes.
                    if let Some(rtext) = v["choices"][0]["delta"]["reasoning_content"]
                        .as_str()
                        .or_else(|| v["choices"][0]["delta"]["reasoning"].as_str())
                        .or_else(|| v["choices"][0]["delta"]["reasoning_summary"].as_str())
                        .or_else(|| v["choices"][0]["delta"]["thought"].as_str())
                    {
                        if !rtext.is_empty() {
                            reasoning_emitted_len += rtext.len();
                            yield StreamEvent::Reasoning { text: rtext.to_string() };
                        }
                    }
                    if let Some(text) = v["choices"][0]["delta"]["content"].as_str() {
                        if !text.is_empty() {
                            carry.push_str(text);
                            let mut out = Vec::new();
                            push_split(&mut out, &mut carry, &mut in_think, &mut text_started);
                            for ev in out {
                                match &ev {
                                    StreamEvent::Reasoning { text } => reasoning_emitted_len += text.len(),
                                    StreamEvent::Delta { text } => delta_emitted_len += text.len(),
                                    _ => {}
                                }
                                yield ev;
                            }
                        }
                    }

                    // Tool-call fragments arrive incrementally, keyed by index.
                    if let Some(calls) = v["choices"][0]["delta"]["tool_calls"].as_array() {
                        for tcd in calls {
                            let idx = tcd.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                            // The index comes off the wire, and the loop below
                            // allocates up to it. A provider bug (or a hostile
                            // one) sending `index: 4000000000` would try to grow
                            // the vector to four billion entries and take the app
                            // out of memory. No model emits anywhere near this
                            // many parallel calls, so a cap costs nothing real.
                            const MAX_PARALLEL_CALLS: usize = 256;
                            if idx >= MAX_PARALLEL_CALLS {
                                continue;
                            }
                            while tools_acc.len() <= idx {
                                tools_acc.push(ToolCallAcc::default());
                            }
                            let tool_start = {
                                let slot = &mut tools_acc[idx];
                                if let Some(id) = tcd.get("id").and_then(Value::as_str) {
                                    if !id.is_empty() {
                                        slot.id = id.to_string();
                                    }
                                }
                                if let Some(func) = tcd.get("function") {
                                    if let Some(name) = func.get("name").and_then(Value::as_str) {
                                        if !name.is_empty() {
                                            slot.name = name.to_string();
                                        }
                                    }
                                    if let Some(args) =
                                        func.get("arguments").and_then(Value::as_str)
                                    {
                                        slot.arguments.push_str(args);
                                    }
                                }
                                // Announce the call once its name is known, so the
                                // UI can ground the wait while args still generate.
                                if !slot.name.is_empty() && !slot.started {
                                    slot.started = true;
                                    Some(slot.name.clone())
                                } else {
                                    None
                                }
                            };
                            if let Some(name) = tool_start {
                                yield StreamEvent::ToolStart { name };
                            }
                        }
                    }

                    if let Some(reason) = v["choices"][0]["finish_reason"].as_str() {
                        finish_reason = Some(reason.to_string());
                        break 'outer;
                    }
                }
            }

            // Flush any held-back content (e.g. a trailing partial tag).
            if !carry.is_empty() {
                yield if in_think {
                    StreamEvent::Reasoning { text: carry }
                } else {
                    StreamEvent::Delta { text: carry }
                };
            }

            // Enforce sequential tool calls if enabled: truncate to 1 call
            if sequential && tools_acc.len() > 1 {
                tools_acc.truncate(1);
            }

            // Flush any accumulated tool calls, then signal completion.
            let has_tools = !tools_acc.is_empty();
            if has_tools {
                eprintln!("[lmstudio] flushing {} tool call(s)", tools_acc.len());
            }
            for (i, slot) in tools_acc.into_iter().enumerate() {
                let id = if slot.id.is_empty() {
                    format!("call_{}", i)
                } else {
                    slot.id
                };
                let arguments = if slot.arguments.trim().is_empty() {
                    "{}".to_string()
                } else {
                    slot.arguments
                };
                yield StreamEvent::ToolCall(ToolCall {
                    id,
                    name: slot.name,
                    arguments,
                });
            }
            let final_finish = finish_reason.or_else(|| {
                if has_tools {
                    Some("tool_calls".into())
                } else {
                    None
                }
            });
            yield StreamEvent::Done { finish_reason: final_finish };
        };

        Ok(Box::pin(s))
    }

    async fn list_models(&self) -> anyhow::Result<Vec<String>> {
        let url = format!("{}/models", self.base_url);
        let mut req = self.http.get(&url);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }
        let v: Value = req.send().await?.json().await?;
        let ids = v["data"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| m["id"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        Ok(ids)
    }
}

#[cfg(test)]
mod think_split_tests {
    use super::push_split;
    use crate::provider::StreamEvent;

    /// Feed content chunks through push_split as the stream would, flush the leftover
    /// carry the way end-of-stream does, and return (reasoning, answer).
    fn split(chunks: &[&str]) -> (String, String) {
        let mut carry = String::new();
        let mut in_think = false;
        let mut text_started = false;
        let mut out = Vec::new();
        for c in chunks {
            carry.push_str(c);
            push_split(&mut out, &mut carry, &mut in_think, &mut text_started);
        }
        if !carry.is_empty() {
            out.push(if in_think {
                StreamEvent::Reasoning { text: carry.clone() }
            } else {
                StreamEvent::Delta { text: carry.clone() }
            });
        }
        let (mut reasoning, mut answer) = (String::new(), String::new());
        for ev in out {
            match ev {
                StreamEvent::Reasoning { text } => reasoning.push_str(&text),
                StreamEvent::Delta { text } => answer.push_str(&text),
                _ => {}
            }
        }
        (reasoning, answer)
    }

    #[test]
    fn leading_think_block_splits() {
        let (r, a) = split(&["<think>", "the reasoning", "</think>", "the answer"]);
        assert_eq!(r, "the reasoning");
        assert_eq!(a, "the answer");
    }

    #[test]
    fn think_after_text_is_literal() {
        // The reported artifact: a model quotes a `<think>` tag mid-answer. It must
        // render as text, not open a reasoning block mid-sentence.
        let (r, a) = split(&["DeepSeek uses `", "<think>", "` inline."]);
        assert_eq!(r, "");
        assert_eq!(a, "DeepSeek uses `<think>` inline.");
    }

    #[test]
    fn full_literal_tag_pair_after_text_stays_text() {
        let (r, a) = split(&["Note the ", "<think>x</think>", " form."]);
        assert_eq!(r, "");
        assert_eq!(a, "Note the <think>x</think> form.");
    }

    #[test]
    fn plain_text_no_tags() {
        let (r, a) = split(&["just ", "a plain ", "answer"]);
        assert_eq!(r, "");
        assert_eq!(a, "just a plain answer");
    }

    #[test]
    fn leading_think_split_across_chunks() {
        let (r, a) = split(&["<thi", "nk>reason", "</think>done"]);
        assert_eq!(r, "reason");
        assert_eq!(a, "done");
    }

    #[test]
    fn leading_whitespace_then_think() {
        let (r, a) = split(&["\n<think>r</think>a"]);
        assert_eq!(r, "r");
        assert_eq!(a, "a");
    }
}

#[cfg(test)]
mod openai_shape_tests {
    use super::to_openai_messages;
    use crate::provider::{Message, Role};

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

    fn roles(out: &[serde_json::Value]) -> Vec<&str> {
        out.iter()
            .map(|m| m["role"].as_str().unwrap_or("?"))
            .collect()
    }

    /// System messages are mapped 1:1 in their original positions.
    /// The head stays at the top, and the tail stays at the end of the context.
    #[test]
    fn system_turns_stay_in_original_positions() {
        let out = to_openai_messages(&[
            msg(Role::System, "head"),
            msg(Role::User, "hi"),
            msg(Role::Assistant, "hey"),
            msg(Role::System, "tail part 1"),
            msg(Role::System, "tail part 2"),
        ]);

        assert_eq!(
            roles(&out),
            vec!["system", "user", "assistant", "system", "system"],
            "system messages should map 1:1 in their original positions"
        );
        assert_eq!(out[0]["content"], "head");
        assert_eq!(out[3]["content"], "tail part 1");
        assert_eq!(out[4]["content"], "tail part 2");
    }

    /// The compaction handover note rides as a system message substituted over
    /// the oldest turns (useCompactionManager `forModel`).
    #[test]
    fn mid_history_system_note_stays_in_place() {
        let out = to_openai_messages(&[
            msg(Role::System, "head"),
            msg(Role::System, "CONTEXT NOTE — earlier conversation compacted"),
            msg(Role::User, "hi"),
            msg(Role::Assistant, "hey"),
            msg(Role::System, "TAIL"),
        ]);

        assert_eq!(roles(&out), vec!["system", "system", "user", "assistant", "system"]);
        assert_eq!(out[0]["content"], "head");
        assert_eq!(out[1]["content"], "CONTEXT NOTE — earlier conversation compacted");
        assert_eq!(out[4]["content"], "TAIL");
    }

    #[test]
    fn chat_request_deserializes_sampling_params() {
        let json_data = serde_json::json!({
            "model": "qwen2.5-coder-7b-instruct",
            "messages": [],
            "temperature": 0.35,
            "top_p": 0.85,
            "max_tokens": 512
        });
        let req: crate::provider::ChatRequest = serde_json::from_value(json_data).unwrap();
        assert_eq!(req.temperature, Some(0.35));
        assert_eq!(req.top_p, Some(0.85));
        assert_eq!(req.max_tokens, Some(512));
    }

    #[test]
    fn active_turn_replays_reasoning_content_historical_turn_omits_it() {
        use super::to_openai_messages_with_opts;

        let mut active_turn_msg = msg(Role::Assistant, "Running tool...");
        active_turn_msg.reasoning = Some("Thinking about code structure...".into());
        active_turn_msg.created_at = None; // Created live in agent.rs during active turn

        let mut historical_turn_stripped = msg(Role::Assistant, "Prior turn answer.");
        historical_turn_stripped.reasoning = None; // Stripped by expand_messages_with_segments
        historical_turn_stripped.created_at = Some(1790000000000);

        let mut historical_turn_retained = msg(Role::Assistant, "Prior turn answer.");
        historical_turn_retained.reasoning = Some("Thinking from turn 1...".into()); // Retained by rolling reasoning window
        historical_turn_retained.created_at = Some(1790000000000);

        // Local / LM Studio path (include_reasoning = true)
        let out = to_openai_messages_with_opts(&[
            active_turn_msg.clone(),
            historical_turn_stripped.clone(),
            historical_turn_retained.clone(),
        ], true);
        assert_eq!(
            out[0].get("reasoning_content").and_then(|v| v.as_str()),
            Some("Thinking about code structure..."),
            "active turn MUST carry reasoning_content to prevent model amnesia"
        );
        assert!(
            out[1].get("reasoning_content").is_none(),
            "historical turns with stripped reasoning MUST NOT replay reasoning_content"
        );
        assert_eq!(
            out[2].get("reasoning_content").and_then(|v| v.as_str()),
            Some("Thinking from turn 1..."),
            "historical turn with retained reasoning MUST carry reasoning_content"
        );

        // Strict OpenAI path (include_reasoning = false)
        let out_openai = to_openai_messages_with_opts(&[
            active_turn_msg,
            historical_turn_stripped,
            historical_turn_retained,
        ], false);
        assert!(
            out_openai[0].get("reasoning_content").is_none(),
            "strict openai path must never send reasoning_content"
        );
        assert!(
            out_openai[1].get("reasoning_content").is_none(),
            "strict openai path must never send reasoning_content"
        );
        assert!(
            out_openai[2].get("reasoning_content").is_none(),
            "strict openai path must never send reasoning_content"
        );
    }

    #[test]
    fn chat_request_deserializes_sequential_tool_calls() {
        let json_data = serde_json::json!({
            "model": "ling-3.0-tiny",
            "messages": [],
            "sequential_tool_calls": true
        });
        let req: crate::provider::ChatRequest = serde_json::from_value(json_data).unwrap();
        assert_eq!(req.sequential_tool_calls, Some(true));

        let json_data_false = serde_json::json!({
            "model": "ling-3.0-tiny",
            "messages": [],
            "sequential_tool_calls": false
        });
        let req_false: crate::provider::ChatRequest = serde_json::from_value(json_data_false).unwrap();
        assert_eq!(req_false.sequential_tool_calls, Some(false));

        let json_data_none = serde_json::json!({
            "model": "ling-3.0-tiny",
            "messages": []
        });
        let req_none: crate::provider::ChatRequest = serde_json::from_value(json_data_none).unwrap();
        assert_eq!(req_none.sequential_tool_calls, None);
    }

    #[test]
    fn test_safe_unemitted_suffix() {
        use super::safe_unemitted_suffix;

        assert_eq!(safe_unemitted_suffix("hello world", 0), "hello world");
        assert_eq!(safe_unemitted_suffix("hello world", 6), "world");
        assert_eq!(safe_unemitted_suffix("hello world", 11), "");
        assert_eq!(safe_unemitted_suffix("hello world", 20), "");

        // Multi-byte UTF-8 handling: "你好世界" is 12 bytes (3 bytes per char)
        let s = "你好世界";
        assert_eq!(safe_unemitted_suffix(s, 0), "你好世界");
        assert_eq!(safe_unemitted_suffix(s, 3), "好世界");
        // Landing inside '你' (byte 1 or 2) should advance safely to byte 3 ('好')
        assert_eq!(safe_unemitted_suffix(s, 1), "好世界");
        assert_eq!(safe_unemitted_suffix(s, 2), "好世界");
        assert_eq!(safe_unemitted_suffix(s, 12), "");
    }

    #[test]
    fn test_extract_reasoning_and_answer() {
        use super::extract_reasoning_and_answer;

        // Dedicated reasoning field present
        let (r, a) = extract_reasoning_and_answer("my thoughts", "my answer");
        assert_eq!(r, "my thoughts");
        assert_eq!(a, "my answer");

        // Leading <think>...</think> in content
        let (r, a) = extract_reasoning_and_answer("", "<think>inner thought</think>final answer");
        assert_eq!(r, "inner thought");
        assert_eq!(a, "final answer");

        // Leading whitespace then <think>...</think>
        let (r, a) = extract_reasoning_and_answer("", "\n  <think>inner thought</think>\nfinal answer");
        assert_eq!(r, "inner thought");
        assert_eq!(a, "\nfinal answer");

        // Prefilled <think> in prompt: generation begins inside thought and only emits </think>
        let (r, a) = extract_reasoning_and_answer("", "started in prefill</think>actual reply");
        assert_eq!(r, "started in prefill");
        assert_eq!(a, "actual reply");

        // Cut off mid-thought with <think> but no </think>
        let (r, a) = extract_reasoning_and_answer("", "<think>incomplete thought");
        assert_eq!(r, "incomplete thought");
        assert_eq!(a, "");

        // Plain answer with no tags
        let (r, a) = extract_reasoning_and_answer("", "plain answer without reasoning");
        assert_eq!(r, "");
        assert_eq!(a, "plain answer without reasoning");

        // Literal <think> tag occurring mid-sentence in answer prose
        let (r, a) = extract_reasoning_and_answer("", "The documentation describes <think>foo</think> as an example.");
        assert_eq!(r, "");
        assert_eq!(a, "The documentation describes <think>foo</think> as an example.");
    }

    #[test]
    fn test_extract_valid_json_args() {
        use super::extract_valid_json_args;

        // Clean JSON object
        assert_eq!(
            extract_valid_json_args(r#"{"path":"pyproject.toml"}"#),
            Some(r#"{"path":"pyproject.toml"}"#.to_string())
        );

        // JSON object with trailing closing brace (the finalized tool call 0 case)
        assert_eq!(
            extract_valid_json_args(r#"{"path":"pyproject.toml"}}"#),
            Some(r#"{"path":"pyproject.toml"}"#.to_string())
        );

        // JSON object with trailing newlines and stop tags
        assert_eq!(
            extract_valid_json_args("{\"path\":\"pyproject.toml\"}\n</tool_call>"),
            Some(r#"{"path":"pyproject.toml"}"#.to_string())
        );

        // Incomplete JSON object
        assert_eq!(extract_valid_json_args(r#"{"path":"pyproj"#), None);

        // Non-object / non-array primitives should not be treated as tool args
        assert_eq!(extract_valid_json_args(r#""just a string""#), None);
        assert_eq!(extract_valid_json_args("12345"), None);

        // Empty / whitespace
        assert_eq!(extract_valid_json_args(""), None);
        assert_eq!(extract_valid_json_args("   \n\t  "), None);
    }

    #[test]
    fn test_salvage_tools_from_diff_error() {
        let raw_err = r#"{"error":"Engine protocol predict stream returned an error: {\"code\":500,\"message\":\"Invalid diff: now finding less tool calls!\n  Previous (2):\n    - name: 'read_file', args: '{\"path\":\"pyproject.toml\"}'\n    - name: 'code_search', args: '{'\n  Current (1):\n    - name: 'read_file', args: '{\"path\":\"pyproject.toml\"}'\n  Current msg text content:\nLet me now look at the project's dependency file and any other security-relevant areas I haven't yet examined.\n<tool_call>code_search\n<arg_key>glob\n\",\"type\":\"server_error\"}"}"#;

        let (text, tools) = super::salvage_tools_from_diff_error(raw_err);
        assert_eq!(
            text.as_deref(),
            Some("Let me now look at the project's dependency file and any other security-relevant areas I haven't yet examined.")
        );
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "read_file");
        assert_eq!(tools[0].arguments, "{\"path\":\"pyproject.toml\"}");
    }
}
