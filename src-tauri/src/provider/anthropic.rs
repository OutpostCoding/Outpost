// ── surface: backend ──────────────────────────────────
// Anthropic (Claude) Messages API backend (raw reqwest, extended-thinking replay).
// Exports: AnthropicProvider · Related: provider/mod.rs, agent.rs

//! Anthropic (Claude) backend — the native Messages API, translated to and from
//! the same normalized Provider interface as the OpenAI-compatible adapter. Raw
//! HTTP via reqwest (no official Rust SDK). Wire format per the claude-api skill:
//! `x-api-key` + `anthropic-version` headers, top-level `system`, `tool_use` /
//! `tool_result` content blocks, and SSE deltas (text / thinking / input_json).
//! EVERY system message — the head, a compaction handover note, and the
//! per-turn tail parts (agent.rs `append_tail`) — folds into the top-level
//! `system` string: one contiguous block before `messages[0]`, on every model.
//! No in-array system entries, no user-role fallback (2026-08-30 — see
//! NOTES.md "single system block"; supersedes the 2026-08-28 in-array design).
//!
//! Extended thinking (adaptive, summarized) is enabled for models that support
//! it (Haiku 4.5 / legacy don't). With tools, Anthropic requires the turn's
//! thinking block (text + signature) replayed alongside each `tool_use`;
//! `to_anthropic` rebuilds it from the `reasoning` + `reasoning_signature`
//! captured during streaming, so Claude can reason through agentic turns too.

use std::collections::HashMap;

use async_stream::stream;
use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::{json, Map, Value};

use super::{ChatRequest, EventStream, Message, Provider, Role, StreamEvent, Tool, ToolCall, wire_dump};

const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Output cap when the request doesn't set one. A ceiling, not a charge — only
/// generated tokens bill — but bounded so a runaway turn stays cheap.
const DEFAULT_MAX_TOKENS: u32 = 8192;
/// Mid-stream idle timeout: once streaming has begun, if no bytes arrive for this
/// long, the upstream connection has stalled (e.g. inference node crashed or
/// silent TCP drop). Surface a clean error rather than hanging indefinitely.
const STREAM_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(75);

pub struct AnthropicProvider {
    base_url: String,
    api_key: String,
    http: reqwest::Client,
}

impl AnthropicProvider {
    /// `base_url` is the API root (e.g. `https://api.anthropic.com`); the Messages
    /// and Models paths are appended. `api_key` is sent as `x-api-key`.
    /// Note: `_mid_system_tail` is retained for caller signature compatibility; all system
    /// turns fold into the single leading system block per the 2026-08-30 architecture.
    pub fn new(base_url: impl Into<String>, api_key: String, _mid_system_tail: bool) -> Self {
        Self {
            base_url: base_url.into(),
            api_key,
            http: crate::provider::http_client(),
        }
    }
}

/// Split a `data:<media_type>;base64,<data>` URI into (media_type, raw base64).
/// Falls back to image/jpeg if the prefix is odd (composer images are always JPEG
/// data URIs, but this stays defensive).
fn parse_data_uri(uri: &str) -> (&str, &str) {
    uri.strip_prefix("data:")
        .and_then(|rest| rest.split_once(','))
        .map(|(meta, data)| (meta.split(';').next().unwrap_or("image/jpeg"), data))
        .unwrap_or(("image/jpeg", uri))
}

/// Split normalized messages into Anthropic's top-level `system` string and its
/// `messages` array. EVERY system turn — the head, a compaction handover note,
/// and the per-turn tail parts (agent.rs `append_tail`) — folds into the
/// top-level `system` string, in order, before `messages[0]`; the array then
/// holds only the transcript. Anthropic concatenates all system content into
/// one contiguous block at the start of context, so the model sees a single
/// system block (2026-08-30 — see NOTES.md "single system block"). This
/// supersedes the 2026-08-28 in-array design: mid-history system entries and
/// the user-role fallback for legacy models are both gone — the top-level
/// string is accepted by every model, so the tail no longer masquerades as
/// user speech on any of them.
///
/// Assistant tool calls become `tool_use` blocks; tool results become
/// `tool_result` blocks grouped into the preceding user turn (Anthropic expects
/// all results from one assistant turn in a single following user message).
fn to_anthropic(messages: &[Message], require_signature: bool) -> (String, Vec<Value>) {
    let mut system_parts: Vec<String> = Vec::new();
    let mut out: Vec<Value> = Vec::new();
    for m in messages {
        match m.role {
            Role::System => {
                if !m.content.is_empty() {
                    system_parts.push(m.content.clone());
                }
            }
            Role::User => {
                // Vision: Claude's content-block array (text + one image block per
                // attachment). Our images are `data:…;base64,…` URIs; Anthropic wants
                // the media_type + raw base64 split out.
                let mut parts: Vec<Value> = Vec::new();
                if !m.content.is_empty() {
                    parts.push(json!({ "type": "text", "text": m.content }));
                }
                for img in &m.images {
                    let (media_type, data) = parse_data_uri(img);
                    parts.push(json!({
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": media_type,
                            "data": data,
                        },
                    }));
                }
                // A tool-result group is ALREADY an array-content user turn (see the
                // Role::Tool arm below). A user turn landing straight after one — our
                // synthetic image-delivery turn, agent.rs — must MERGE into it rather
                // than open a second consecutive user message, which Anthropic rejects
                // (user/assistant must alternate). Merging yields one user turn of
                // [tool_result…, text, image], which is exactly what we want the model
                // to see: "here are your tool results, and here is the screenshot".
                let merged = if parts.is_empty() {
                    out.last().and_then(|l| l.get("role")).and_then(Value::as_str) == Some("user")
                } else if let Some(last) = out.last_mut().filter(|l| l.get("role").and_then(Value::as_str) == Some("user")) {
                    if let Some(arr) = last.get_mut("content").and_then(Value::as_array_mut) {
                        arr.append(&mut parts);
                        true
                    } else if let Some(prev_str) = last.get("content").and_then(Value::as_str) {
                        if m.images.is_empty() {
                            let new_content = if prev_str.is_empty() {
                                m.content.clone()
                            } else if m.content.is_empty() {
                                prev_str.to_string()
                            } else {
                                format!("{prev_str}\n\n{}", m.content)
                            };
                            last["content"] = json!(new_content);
                            true
                        } else {
                            let mut arr = Vec::new();
                            if !prev_str.is_empty() {
                                arr.push(json!({ "type": "text", "text": prev_str }));
                            }
                            arr.append(&mut parts);
                            last["content"] = json!(arr);
                            true
                        }
                    } else {
                        false
                    }
                } else {
                    false
                };
                if merged {
                    continue;
                }
                if m.images.is_empty() {
                    // Ordinary text turn keeps the plain-string shape (unchanged).
                    out.push(json!({ "role": "user", "content": m.content }));
                } else {
                    out.push(json!({ "role": "user", "content": parts }));
                }
            }
            Role::Assistant => {
                let mut blocks: Vec<Value> = Vec::new();
                // Replay the preserved thinking block first — Anthropic requires it
                // beside this turn's tool_use when continuing with thinking on.
                // For official Anthropic endpoints, Anthropic requires both thinking text
                // and a valid HMAC signature; if signature is missing Anthropic rejects the
                // call with HTTP 400. For non-Anthropic proxies (e.g. Xiaomi MiMo) which
                // stream thinking_delta without signature_delta, replay the thinking block
                // without requiring signature, allowing the model to see its prior reasoning.
                if let Some(reasoning) = &m.reasoning {
                    if !reasoning.is_empty() {
                        if let Some(sig) = &m.reasoning_signature {
                            if !sig.is_empty() {
                                blocks.push(json!({
                                    "type": "thinking",
                                    "thinking": reasoning,
                                    "signature": sig,
                                }));
                            } else if !require_signature {
                                blocks.push(json!({
                                    "type": "thinking",
                                    "thinking": reasoning,
                                }));
                            }
                        } else if !require_signature {
                            blocks.push(json!({
                                "type": "thinking",
                                "thinking": reasoning,
                            }));
                        }
                    }
                }
                if !m.content.is_empty() {
                    blocks.push(json!({ "type": "text", "text": m.content }));
                }
                for tc in &m.tool_calls {
                    let input: Value =
                        serde_json::from_str(&tc.arguments).unwrap_or_else(|_| json!({}));
                    blocks.push(json!({
                        "type": "tool_use",
                        "id": tc.id,
                        "name": tc.name,
                        "input": input,
                    }));
                }
                if blocks.is_empty() {
                    blocks.push(json!({ "type": "text", "text": "" }));
                }

                // If the previous turn is already an assistant message, merge blocks into it
                // rather than opening a second consecutive assistant message.
                if let Some(last) = out.last_mut().filter(|l| l.get("role").and_then(Value::as_str) == Some("assistant")) {
                    if let Some(arr) = last.get_mut("content").and_then(Value::as_array_mut) {
                        for b in blocks {
                            let is_empty_text = b.get("type").and_then(Value::as_str) == Some("text")
                                && b.get("text").and_then(Value::as_str).map_or(false, str::is_empty);
                            if !is_empty_text || arr.is_empty() {
                                arr.push(b);
                            }
                        }
                        continue;
                    }
                }

                out.push(json!({ "role": "assistant", "content": blocks }));
            }
            Role::Tool => {
                let block = json!({
                    "type": "tool_result",
                    "tool_use_id": m.tool_call_id.clone().unwrap_or_default(),
                    "content": m.content,
                });
                // Append to the previous user turn if it's already a results group
                // (array content); otherwise open a new user turn for the results.
                if let Some(last) = out.last_mut() {
                    if last.get("role").and_then(Value::as_str) == Some("user") {
                        if let Some(arr) = last.get_mut("content").and_then(Value::as_array_mut) {
                            arr.push(block);
                            continue;
                        } else if let Some(prev_str) = last.get("content").and_then(Value::as_str) {
                            let mut arr = Vec::new();
                            if !prev_str.is_empty() {
                                arr.push(json!({ "type": "text", "text": prev_str }));
                            }
                            arr.push(block);
                            last["content"] = json!(arr);
                            continue;
                        }
                    }
                }
                out.push(json!({ "role": "user", "content": [block] }));
            }
        }
    }
    (system_parts.join("\n\n"), out)
}

fn to_anthropic_tools(tools: &[Tool], enable_cache: bool) -> Vec<Value> {
    let len = tools.len();
    tools
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let mut obj = json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t.parameters,
            });
            if enable_cache && i + 1 == len {
                obj.as_object_mut()
                    .unwrap()
                    .insert("cache_control".into(), json!({ "type": "ephemeral" }));
            }
            obj
        })
        .collect()
}

/// Attach `cache_control: { type: "ephemeral" }` to a message.
/// Anthropic requires `cache_control` to be placed on a content block (not a plain string).
fn apply_cache_control_to_message(msg: &mut Value) {
    if let Some(obj) = msg.as_object_mut() {
        if let Some(content) = obj.get_mut("content") {
            if let Some(text) = content.as_str() {
                if !text.is_empty() {
                    *content = json!([
                        {
                            "type": "text",
                            "text": text,
                            "cache_control": { "type": "ephemeral" }
                        }
                    ]);
                }
            } else if let Some(arr) = content.as_array_mut() {
                if let Some(last_block) = arr.last_mut() {
                    if let Some(block_obj) = last_block.as_object_mut() {
                        block_obj.insert("cache_control".into(), json!({ "type": "ephemeral" }));
                    }
                }
            }
        }
    }
}

/// Inject Anthropic prompt-caching breakpoints (`cache_control: { "type": "ephemeral" }`).
/// Anthropic allows up to 4 breakpoints per request:
/// 1. `system`: 1 breakpoint on the system prompt block.
/// 2. `tools`: 1 breakpoint on the last tool in `tools`.
/// 3. `messages`: up to 2 breakpoints on conversation history:
///    - The last user turn (so the subsequent turn gets an immediate cache hit).
///    - The second-to-last user turn (so the current turn gets a cache hit from the previous round).
fn apply_cache_control_breakpoints(
    system: &str,
    tools: &[Tool],
    messages: &mut [Value],
) -> (Option<Value>, Option<Vec<Value>>) {
    let system_value = if !system.is_empty() {
        Some(json!([
            {
                "type": "text",
                "text": system,
                "cache_control": { "type": "ephemeral" }
            }
        ]))
    } else {
        None
    };

    let tools_value = if !tools.is_empty() {
        Some(to_anthropic_tools(tools, true))
    } else {
        None
    };

    let user_indices: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.get("role").and_then(Value::as_str) == Some("user"))
        .map(|(i, _)| i)
        .collect();

    if let Some(&last_idx) = user_indices.last() {
        apply_cache_control_to_message(&mut messages[last_idx]);
    }
    if user_indices.len() >= 2 {
        let second_last_idx = user_indices[user_indices.len() - 2];
        apply_cache_control_to_message(&mut messages[second_last_idx]);
    }

    (system_value, tools_value)
}

/// Whether a model supports adaptive extended thinking + `output_config.effort`.
/// Per the claude-api skill: Fable 5, Opus 4.8/4.7/4.6, Sonnet 5, and Sonnet 4.6
/// do; Haiku 4.5 and the legacy/older models reject the `thinking` param (400),
/// so we omit it for them. NB `sonnet-5` matches "claude-sonnet-5" but NOT
/// "claude-sonnet-4-5" (that's "sonnet-4-5") — the legacy Sonnet stays excluded.
fn supports_adaptive_thinking(model: &str) -> bool {
    model.contains("fable")
        || model.contains("mythos")
        || model.contains("opus-4-8")
        || model.contains("opus-4-7")
        || model.contains("opus-4-6")
        || model.contains("sonnet-5")
        || model.contains("sonnet-4-6")
}

/// Fable/Mythos think unconditionally and reject an explicit `{type:"disabled"}`
/// (400). "Off" for these can only omit the `thinking` param — the model still
/// thinks, but the summary isn't returned (so nothing renders).
fn thinking_always_on(model: &str) -> bool {
    model.contains("fable") || model.contains("mythos")
}

#[async_trait]
impl Provider for AnthropicProvider {
    async fn stream_chat(&self, request: ChatRequest) -> anyhow::Result<EventStream> {
        let url = format!("{}/v1/messages", self.base_url);
        let is_mimo = self.base_url.contains("mimo") || self.base_url.contains("xiaomi");
        let require_signature = self.base_url.contains("anthropic.com") && !is_mimo;
        let (system, mut messages) = to_anthropic(&request.messages, require_signature);
        wire_dump(Some(&system), &messages);

        let (system_val, tools_val) =
            apply_cache_control_breakpoints(&system, &request.tools, &mut messages);

        let default_max = if is_mimo || supports_adaptive_thinking(&request.model) {
            32768
        } else {
            DEFAULT_MAX_TOKENS
        };

        let mut body = Map::new();
        body.insert("model".into(), json!(request.model));
        body.insert(
            "max_tokens".into(),
            json!(request.max_tokens.unwrap_or(default_max)),
        );
        body.insert("messages".into(), json!(messages));
        body.insert("stream".into(), json!(true));
        if let Some(sys) = system_val {
            body.insert("system".into(), sys);
        }
        if let Some(tls) = tools_val {
            body.insert("tools".into(), json!(tls));
        }
        // Thinking + effort. Adaptive thinking self-regulates; `output_config.effort`
        // sets its depth (server default "high"). The UI's Thinking selector arrives
        // as `thinking_effort`: "off" disables it, an effort name enables adaptive at
        // that depth, None → adaptive at the default. Models that reject the param
        // (Haiku 4.5 / legacy) get neither. IMPORTANT: on Opus 4.7/4.8 (and this is
        // what makes thinking silently vanish), OMITTING `thinking` means NO thinking
        // — it must be sent explicitly to be on. Fable/Mythos think unconditionally
        // and reject {type:"disabled"}, so "off" there omits the param (hiding the
        // summary) rather than turning thinking off. Adaptive works alongside tools —
        // the thinking block is captured and replayed (see to_anthropic). Sampling
        // params are omitted: current Claude models reject them.
        let effort = request.thinking_effort.as_deref().unwrap_or("high");
        if supports_adaptive_thinking(&request.model) {
            if effort == "off" {
                if !thinking_always_on(&request.model) {
                    body.insert("thinking".into(), json!({ "type": "disabled" }));
                }
            } else {
                body.insert(
                    "thinking".into(),
                    json!({ "type": "adaptive", "display": "summarized" }),
                );
                body.insert("output_config".into(), json!({ "effort": effort }));
            }
        }

        let beta_header = if default_max > 8192 {
            "prompt-caching-2024-07-31,output-128k-2025-02-19"
        } else {
            "prompt-caching-2024-07-31"
        };

        let mut req = self
            .http
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("anthropic-beta", beta_header)
            .json(&Value::Object(body));

        if is_mimo {
            req = req.header("authorization", format!("Bearer {}", self.api_key));
        }

        let resp = req.send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let raw = resp.text().await.unwrap_or_default();
            let trimmed = raw.trim();
            // Anthropic errors are JSON ({error:{message}}); surface that message,
            // else a truncated raw body.
            let detail = serde_json::from_str::<Value>(trimmed)
                .ok()
                .and_then(|v| v["error"]["message"].as_str().map(String::from))
                .unwrap_or_else(|| {
                    let short: String = trimmed.chars().take(500).collect();
                    if trimmed.chars().count() > 500 {
                        format!("{short}…")
                    } else {
                        short
                    }
                });
            anyhow::bail!("Anthropic {status}: {detail}");
        }

        let mut bytes = resp.bytes_stream();
        let s = stream! {
            let mut buf: Vec<u8> = Vec::new();
            // tool_use blocks accumulate by content-block index; `order` preserves
            // emission order for the flush at the end.
            let mut tools_acc: HashMap<usize, (String, String, String)> = HashMap::new();
            let mut order: Vec<usize> = Vec::new();
            let mut stop_reason: Option<String> = None;
            let mut any_text = false;
            let mut sig_acc = String::new();

            let mut started = false;
            'outer: loop {
                let next_chunk = if started {
                    match tokio::time::timeout(STREAM_IDLE_TIMEOUT, bytes.next()).await {
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
                    }
                } else {
                    bytes.next().await
                };

                let reached_eof = match next_chunk {
                    Some(Ok(b)) => {
                        started = true;
                        buf.extend_from_slice(&b);
                        false
                    }
                    Some(Err(e)) => {
                        yield StreamEvent::Error { message: format!("stream error: {e}") };
                        return;
                    }
                    None => {
                        if buf.is_empty() {
                            break 'outer;
                        }
                        buf.push(b'\n');
                        true
                    }
                };

                // SSE frames are newline-delimited; decode whole lines only so a
                // multi-byte char split across chunks isn't corrupted.
                while let Some(nl) = buf.iter().position(|&c| c == b'\n') {
                    let line_bytes: Vec<u8> = buf.drain(..=nl).collect();
                    let line = String::from_utf8_lossy(&line_bytes);
                    let line = line.trim();
                    let data = if let Some(d) = line.strip_prefix("data:") {
                        d.trim()
                    } else if line.starts_with('{') && line.contains("\"error\"") {
                        line
                    } else {
                        continue;
                    };
                    if data.is_empty() {
                        continue;
                    }
                    let Ok(v) = serde_json::from_str::<Value>(data) else {
                        continue;
                    };

                    match v["type"].as_str() {
                        Some("message_start") => {
                            if let Some(usage) = v["message"]["usage"].as_object() {
                                let input = usage.get("input_tokens").and_then(Value::as_u64).unwrap_or(0);
                                let cache_read = usage.get("cache_read_input_tokens").and_then(Value::as_u64).unwrap_or(0);
                                let cache_create = usage.get("cache_creation_input_tokens").and_then(Value::as_u64).unwrap_or(0);
                                if cache_read > 0 || cache_create > 0 {
                                    eprintln!(
                                        "[anthropic] prompt cache: read={cache_read}, created={cache_create}, input={input}"
                                    );
                                }
                            }
                        }
                        Some("content_block_start") => {
                            let idx = v["index"].as_u64().unwrap_or(0) as usize;
                            let cb = &v["content_block"];
                            if cb["type"].as_str() == Some("tool_use") {
                                let id = cb["id"].as_str().unwrap_or("").to_string();
                                let name = cb["name"].as_str().unwrap_or("").to_string();
                                tools_acc.insert(idx, (id, name.clone(), String::new()));
                                order.push(idx);
                                yield StreamEvent::ToolStart { name };
                            }
                        }
                        Some("content_block_delta") => {
                            let idx = v["index"].as_u64().unwrap_or(0) as usize;
                            let d = &v["delta"];
                            match d["type"].as_str() {
                                Some("text_delta") => {
                                    if let Some(t) = d["text"].as_str() {
                                        if !t.is_empty() {
                                            any_text = true;
                                            yield StreamEvent::Delta { text: t.to_string() };
                                        }
                                    }
                                }
                                Some("thinking_delta") => {
                                    if let Some(t) = d["thinking"].as_str() {
                                        if !t.is_empty() {
                                            yield StreamEvent::Reasoning { text: t.to_string() };
                                        }
                                    }
                                }
                                Some("signature_delta") => {
                                    if let Some(s) = d["signature"].as_str() {
                                        sig_acc.push_str(s);
                                    }
                                }
                                Some("input_json_delta") => {
                                    if let Some(pj) = d["partial_json"].as_str() {
                                        if let Some(slot) = tools_acc.get_mut(&idx) {
                                            slot.2.push_str(pj);
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        Some("content_block_stop") => {
                            // End of a block; if it carried a thinking signature, hand
                            // it to the agent loop for tool-use replay.
                            if !sig_acc.is_empty() {
                                yield StreamEvent::ReasoningSignature {
                                    signature: std::mem::take(&mut sig_acc),
                                };
                            }
                        }
                        Some("message_delta") => {
                            if let Some(sr) = v["delta"]["stop_reason"].as_str() {
                                stop_reason = Some(sr.to_string());
                            }
                            if let Some(usage) = v.get("usage").and_then(Value::as_object) {
                                let output = usage.get("output_tokens").and_then(Value::as_u64).unwrap_or(0);
                                if output > 0 {
                                    eprintln!("[anthropic] completion tokens: output={output}");
                                }
                            }
                        }
                        Some("message_stop") => break 'outer,
                        Some("error") => {
                            let msg = v["error"]["message"]
                                .as_str()
                                .or_else(|| v["error"].as_str())
                                .unwrap_or("stream error");
                            yield StreamEvent::Error { message: format!("Anthropic: {msg}") };
                            return;
                        }
                        _ => {
                            if let Some(err) = v.get("error") {
                                let msg = err
                                    .get("message")
                                    .and_then(|m| m.as_str())
                                    .or_else(|| err.as_str())
                                    .unwrap_or("stream error");
                                yield StreamEvent::Error { message: format!("Anthropic: {msg}") };
                                return;
                            }
                        }
                    }
                }

                if reached_eof {
                    break 'outer;
                }
            }

            // A safety refusal can end the turn with no text — say so rather than
            // showing a blank reply.
            if stop_reason.as_deref() == Some("refusal") && !any_text {
                yield StreamEvent::Delta {
                    text: "[The model declined to respond to this request.]".to_string(),
                };
            }

            // Flush accumulated tool calls in start order.
            for idx in order {
                if let Some((id, name, args)) = tools_acc.remove(&idx) {
                    let arguments = if args.trim().is_empty() {
                        "{}".to_string()
                    } else {
                        args
                    };
                    yield StreamEvent::ToolCall(ToolCall { id, name, arguments, extra_content: None });
                }
            }
            yield StreamEvent::Done { finish_reason: stop_reason };
        };

        Ok(Box::pin(s))
    }

    async fn list_models(&self) -> anyhow::Result<Vec<String>> {
        let is_mimo = self.base_url.contains("mimo") || self.base_url.contains("xiaomi");
        let url = format!("{}/v1/models", self.base_url);
        let mut req = self
            .http
            .get(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION);

        if is_mimo {
            req = req.header("authorization", format!("Bearer {}", self.api_key));
        }

        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                if is_mimo {
                    return Ok(vec![
                        "mimo-v2.6-flash".into(),
                        "mimo-v2.6-pro".into(),
                        "mimo-v2.6-pro-ultraspeed".into(),
                    ]);
                }
                return Err(e.into());
            }
        };

        if !resp.status().is_success() {
            if is_mimo {
                return Ok(vec![
                    "mimo-v2.6-flash".into(),
                    "mimo-v2.6-pro".into(),
                    "mimo-v2.6-pro-ultraspeed".into(),
                ]);
            }
            anyhow::bail!("Anthropic {} listing models", resp.status());
        }
        let v: Value = resp.json().await.unwrap_or_default();
        let ids: Vec<String> = v["data"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| m["id"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        if ids.is_empty() && is_mimo {
            return Ok(vec![
                "mimo-v2.6-flash".into(),
                "mimo-v2.6-pro".into(),
                "mimo-v2.6-pro-ultraspeed".into(),
            ]);
        }
        Ok(ids)
    }
}

#[cfg(test)]
mod anthropic_shape_tests {
    use super::*;

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

    fn roles(out: &[Value]) -> Vec<&str> {
        out.iter()
            .map(|m| m["role"].as_str().unwrap_or("?"))
            .collect()
    }

    fn kinds(turn: &Value) -> Vec<&str> {
        turn["content"]
            .as_array()
            .expect("content should be a block array")
            .iter()
            .map(|p| p["type"].as_str().unwrap_or("?"))
            .collect()
    }

    /// THE screenshot-delivery shape: assistant tool_use → its tool_result → the
    /// synthetic user turn carrying the image. Anthropic requires alternating
    /// user/assistant turns, so the image turn MUST merge into the tool_result user
    /// turn instead of following it as a second consecutive user message (a 400).
    #[test]
    fn image_turn_merges_into_tool_result_group() {
        let mut assistant = msg(Role::Assistant, "");
        assistant.tool_calls = vec![ToolCall {
            id: "call_1".into(),
            name: "browser_screenshot".into(),
            arguments: "{}".into(),
            extra_content: None,
        }];
        let mut result = msg(Role::Tool, "screenshot captured");
        result.tool_call_id = Some("call_1".into());
        let mut shot = msg(Role::User, "Screenshot from browser_screenshot.");
        shot.images = vec!["data:image/png;base64,AAAA".into()];

        let (_, out) = to_anthropic(&[assistant, result, shot], true);

        assert_eq!(
            roles(&out),
            vec!["assistant", "user"],
            "must not emit two consecutive user turns"
        );
        assert_eq!(kinds(&out[1]), vec!["tool_result", "text", "image"]);
        assert_eq!(out[1]["content"][2]["source"]["media_type"], "image/png");
        assert_eq!(out[1]["content"][2]["source"]["data"], "AAAA");
    }

    /// Several tool results plus the image still collapse into ONE user turn.
    #[test]
    fn multiple_tool_results_then_image_stay_one_turn() {
        let mut assistant = msg(Role::Assistant, "");
        assistant.tool_calls = vec![
            ToolCall {
                id: "a".into(),
                name: "project_read".into(),
                arguments: "{}".into(),
                extra_content: None,
            },
            ToolCall {
                id: "b".into(),
                name: "browser_screenshot".into(),
                arguments: "{}".into(),
                extra_content: None,
            },
        ];
        let mut r1 = msg(Role::Tool, "file contents");
        r1.tool_call_id = Some("a".into());
        let mut r2 = msg(Role::Tool, "screenshot captured");
        r2.tool_call_id = Some("b".into());
        let mut shot = msg(Role::User, "Screenshot.");
        shot.images = vec!["data:image/png;base64,CCCC".into()];

        let (_, out) = to_anthropic(&[assistant, r1, r2, shot], true);

        assert_eq!(roles(&out), vec!["assistant", "user"]);
        assert_eq!(
            kinds(&out[1]),
            vec!["tool_result", "tool_result", "text", "image"]
        );
    }

    /// A composer image (no preceding tool result) still opens its own user turn —
    /// the existing upload path must be untouched by the merge rule.
    #[test]
    fn composer_image_turn_stands_alone() {
        let mut m = msg(Role::User, "look at this");
        m.images = vec!["data:image/jpeg;base64,BBBB".into()];

        let (_, out) = to_anthropic(&[m], true);

        assert_eq!(roles(&out), vec!["user"]);
        assert_eq!(kinds(&out[0]), vec!["text", "image"]);
    }

    /// The producer/consumer contract: `project_read_image` builds
    /// `data:<media_type>;base64,<payload>`, and this adapter must split it back into
    /// exactly the media_type + raw payload Anthropic wants. Round-trip real bytes
    /// (a PNG magic header, including a non-UTF8 byte) through both halves.
    #[test]
    fn data_uri_round_trips_from_the_image_tool_format() {
        use base64::Engine as _;
        let raw: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0xFF];
        let encoded = base64::engine::general_purpose::STANDARD.encode(raw);

        // Bind the URI: parse_data_uri borrows from it, so a temporary won't live.
        let uri = format!("data:image/png;base64,{encoded}");
        let (media_type, data) = parse_data_uri(&uri);

        assert_eq!(media_type, "image/png");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(data)
            .expect("payload should still be valid base64 after the split");
        assert_eq!(decoded, raw, "image bytes must survive producer → adapter");
    }

    /// Ordinary text turns keep the plain-string content shape (no regression), and
    /// the head hoists into the top-level `system` string.
    #[test]
    fn plain_text_turn_unchanged_and_head_stays_top_level() {
        let (system, out) = to_anthropic(&[msg(Role::System, "be brief"), msg(Role::User, "hi")], true);

        assert_eq!(system, "be brief");
        assert_eq!(roles(&out), vec!["user"]);
        assert_eq!(out[0]["content"], "hi");
    }

    /// A mid-history system turn (the per-turn tail) folds into the top-level
    /// `system` string — head and tail become ONE block before `messages[0]`,
    /// and nothing system-typed remains in the array.
    #[test]
    fn mid_history_system_folds_into_the_top_level_prefix() {
        let (system, out) = to_anthropic(&[
            msg(Role::System, "be brief"),
            msg(Role::User, "hi"),
            msg(Role::System, "TAIL"),
        ], true);

        assert_eq!(system, "be brief\n\nTAIL");
        assert_eq!(roles(&out), vec!["user"]);
        assert_eq!(out[0]["content"], "hi");
    }

    /// Several tail parts and a mid-history handover note all fold into the
    /// prefix, in order — the old user-role fallback is gone, so the tail never
    /// touches the user's words, on any model.
    #[test]
    fn all_system_turns_fold_in_order_and_leave_no_system_entries() {
        let (system, out) = to_anthropic(&[
            msg(Role::System, "head"),
            msg(Role::System, "CONTEXT NOTE — earlier conversation compacted"),
            msg(Role::User, "hi"),
            msg(Role::Assistant, "hey"),
            msg(Role::System, "TAIL 1"),
            msg(Role::System, "TAIL 2"),
        ], true);

        assert_eq!(
            system,
            "head\n\nCONTEXT NOTE — earlier conversation compacted\n\nTAIL 1\n\nTAIL 2"
        );
        assert_eq!(roles(&out), vec!["user", "assistant"]);
        assert_eq!(out[0]["content"], "hi");
    }

    /// After a tool round the tail still goes to the system prefix — it no
    /// longer merges into the results group (the legacy user-role fallback), so
    /// the results stay exactly what the tool returned.
    #[test]
    fn tail_after_tool_result_goes_to_the_prefix_not_the_results_group() {
        let mut assistant = msg(Role::Assistant, "");
        assistant.tool_calls = vec![ToolCall {
            id: "call_1".into(),
            name: "project_read".into(),
            arguments: "{}".into(),
            extra_content: None,
        }];
        let mut result = msg(Role::Tool, "file contents");
        result.tool_call_id = Some("call_1".into());

        let (system, out) = to_anthropic(&[
            msg(Role::System, "be brief"),
            msg(Role::User, "read it"),
            assistant,
            result,
            msg(Role::System, "TAIL"),
        ], true);

        assert_eq!(system, "be brief\n\nTAIL");
        assert_eq!(roles(&out), vec!["user", "assistant", "user"]);
        assert_eq!(kinds(&out[2]), vec!["tool_result"], "results untouched");
        assert_eq!(out[2]["content"][0]["content"], "file contents");
    }

    #[test]
    fn thinking_replay_requires_signature_for_official_anthropic() {
        let mut assistant = msg(Role::Assistant, "I will inspect the file.");
        assistant.reasoning = Some("Thinking about the code structure...".into());
        assistant.reasoning_signature = None;
        assistant.tool_calls = vec![ToolCall {
            id: "call_1".into(),
            name: "project_read".into(),
            arguments: "{}".into(),
            extra_content: None,
        }];

        // When require_signature is true (official Anthropic): no signature means no thinking block (avoids 400)
        let (_, out_official) = to_anthropic(&[assistant.clone()], true);
        assert_eq!(kinds(&out_official[0]), vec!["text", "tool_use"]);

        // When require_signature is false (MiMo / non-Anthropic proxy): thinking is replayed without signature
        let (_, out_mimo) = to_anthropic(&[assistant.clone()], false);
        assert_eq!(kinds(&out_mimo[0]), vec!["thinking", "text", "tool_use"]);
        assert_eq!(
            out_mimo[0]["content"][0]["thinking"],
            "Thinking about the code structure..."
        );
        assert!(out_mimo[0]["content"][0].get("signature").is_none());

        // When signature IS present, both include signature
        assistant.reasoning_signature = Some("sig_xyz".into());
        let (_, out_with_sig) = to_anthropic(&[assistant.clone()], true);
        assert_eq!(kinds(&out_with_sig[0]), vec!["thinking", "text", "tool_use"]);
        assert_eq!(out_with_sig[0]["content"][0]["signature"], "sig_xyz");
    }

    #[test]
    fn prompt_caching_breakpoints_structure_and_cap() {
        let system_text = "You are a coding assistant.";
        let tools = vec![
            Tool {
                name: "tool_1".into(),
                description: "desc 1".into(),
                parameters: json!({}),
            },
            Tool {
                name: "tool_2".into(),
                description: "desc 2".into(),
                parameters: json!({}),
            },
        ];

        let mut messages = vec![
            json!({ "role": "user", "content": "turn 1" }),
            json!({ "role": "assistant", "content": [{ "type": "text", "text": "answer 1" }] }),
            json!({ "role": "user", "content": "turn 2" }),
            json!({ "role": "assistant", "content": [{ "type": "text", "text": "answer 2" }] }),
            json!({ "role": "user", "content": "turn 3" }),
        ];

        let (sys_val, tools_val) =
            apply_cache_control_breakpoints(system_text, &tools, &mut messages);

        // 1. System block carries cache_control
        let sys_arr = sys_val.expect("system should be present");
        assert_eq!(sys_arr[0]["type"], "text");
        assert_eq!(sys_arr[0]["text"], system_text);
        assert_eq!(sys_arr[0]["cache_control"]["type"], "ephemeral");

        // 2. Only the last tool in tools carries cache_control
        let tls = tools_val.expect("tools should be present");
        assert_eq!(tls.len(), 2);
        assert!(tls[0].get("cache_control").is_none());
        assert_eq!(tls[1]["cache_control"]["type"], "ephemeral");

        // 3. In messages, second-to-last user (turn 2, index 2) and last user (turn 3, index 4) carry cache_control
        assert!(messages[0]["content"].is_string(), "turn 1 should not have cache_control");
        assert_eq!(
            messages[2]["content"][0]["cache_control"]["type"],
            "ephemeral",
            "turn 2 (second-to-last user turn) must have cache_control"
        );
        assert_eq!(
            messages[4]["content"][0]["cache_control"]["type"],
            "ephemeral",
            "turn 3 (latest user turn) must have cache_control"
        );

        // 4. Total breakpoints across entire request: 1 (system) + 1 (tools) + 2 (messages) = 4
        let mut total_breakpoints = 0;
        if sys_arr[0].get("cache_control").is_some() {
            total_breakpoints += 1;
        }
        for t in &tls {
            if t.get("cache_control").is_some() {
                total_breakpoints += 1;
            }
        }
        for m in &messages {
            if let Some(arr) = m["content"].as_array() {
                for b in arr {
                    if b.get("cache_control").is_some() {
                        total_breakpoints += 1;
                    }
                }
            }
        }
        assert_eq!(total_breakpoints, 4, "must exactly utilize the 4 allowed breakpoints");
    }

    #[test]
    fn prompt_caching_tool_results_and_single_turn() {
        let tools: Vec<Tool> = Vec::new();
        let mut messages = vec![
            json!({
                "role": "user",
                "content": [
                    { "type": "tool_result", "tool_use_id": "call_1", "content": "result 1" },
                    { "type": "tool_result", "tool_use_id": "call_2", "content": "result 2" },
                ]
            })
        ];

        let (sys_val, tools_val) = apply_cache_control_breakpoints("", &tools, &mut messages);

        assert!(sys_val.is_none(), "empty system yields None");
        assert!(tools_val.is_none(), "empty tools yields None");

        // Single turn: cache_control placed on the final tool_result block in the array
        let arr = messages[0]["content"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert!(arr[0].get("cache_control").is_none());
        assert_eq!(arr[1]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn to_anthropic_merges_consecutive_same_role_messages() {
        // Consecutive user messages
        let msgs_user = vec![
            msg(Role::User, "first user question"),
            msg(Role::User, "second user addendum"),
        ];
        let (_, out_user) = to_anthropic(&msgs_user, true);
        assert_eq!(out_user.len(), 1, "consecutive user turns should merge into 1");
        assert_eq!(out_user[0]["role"], "user");
        assert_eq!(
            out_user[0]["content"],
            "first user question\n\nsecond user addendum"
        );

        // Consecutive assistant messages
        let mut a1 = msg(Role::Assistant, "part 1");
        a1.tool_calls = vec![ToolCall {
            id: "tc1".into(),
            name: "test_tool".into(),
            arguments: "{}".into(),
            extra_content: None,
        }];
        let a2 = msg(Role::Assistant, "part 2 answer");
        let (_, out_asst) = to_anthropic(&[a1, a2], true);
        assert_eq!(out_asst.len(), 1, "consecutive assistant turns should merge into 1");
        assert_eq!(out_asst[0]["role"], "assistant");
        let blocks = out_asst[0]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[0]["text"], "part 1");
        assert_eq!(blocks[1]["type"], "tool_use");
        assert_eq!(blocks[2]["type"], "text");
        assert_eq!(blocks[2]["text"], "part 2 answer");
    }
}
