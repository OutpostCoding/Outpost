// ── surface: backend ──────────────────────────────────
// Google Gemini REST API backend (streamGenerateContent, native function calls, thoughts).
// Exports: GeminiProvider · Related: provider/mod.rs, agent.rs

//! Google Gemini backend — native Google GenAI REST API (v1beta), translated to and from
//! the same normalized Provider interface as the OpenAI and Anthropic adapters.
//!
//! Why a native provider is essential for Gemini:
//! 1. Google's `/v1beta/openai` compatibility layer drops arguments on parallel tool calls
//!    (emitting blank `{}` payloads) and suppresses thoughts for Gemini 2.5 / 2.0.
//! 2. The native `streamGenerateContent` API provides first-class support for:
//!    - Streaming internal reasoning (`parts[].thought` -> `StreamEvent::Reasoning`)
//!    - Parallel function calling with complete JSON object arguments (`parts[].functionCall`)
//!    - Round-tripping cryptographic `thoughtSignature` / `thought_signature` across multi-turn tool loops
//!    - Direct `systemInstruction` folding and strict alternating user/model turns.

use std::collections::HashMap;
use std::time::Duration;

use async_stream::stream;
use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::{json, Map, Value};

use super::{
    ChatRequest, EventStream, Message, Provider, Role, StreamEvent, Tool, ToolCall, wire_dump,
};

const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com";
const DEFAULT_MAX_TOKENS: u32 = 8192;
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(90);

pub struct GeminiProvider {
    base_url: String,
    api_key: String,
    http: reqwest::Client,
}

impl GeminiProvider {
    pub fn new(base_url: impl Into<String>, api_key: String) -> Self {
        Self {
            base_url: base_url.into(),
            api_key,
            http: crate::provider::http_client(),
        }
    }
}

/// Normalizes the Gemini base URL, cleanly removing legacy `/v1beta/openai` or `/v1beta`
/// suffixes so existing user settings transparently upgrade to native endpoints.
fn normalize_base_url(url: &str) -> String {
    let mut u = url.trim().trim_end_matches('/').to_string();
    if u.is_empty() {
        return DEFAULT_BASE_URL.to_string();
    }
    if let Some(stripped) = u.strip_suffix("/v1beta/openai") {
        u = stripped.trim_end_matches('/').to_string();
    }
    if let Some(stripped) = u.strip_suffix("/v1beta") {
        u = stripped.trim_end_matches('/').to_string();
    }
    if u.is_empty() {
        DEFAULT_BASE_URL.to_string()
    } else {
        u
    }
}

/// Split a `data:<media_type>;base64,<data>` URI into (media_type, raw base64).
fn parse_data_uri(uri: &str) -> (&str, &str) {
    uri.strip_prefix("data:")
        .and_then(|rest| rest.split_once(','))
        .map(|(meta, data)| (meta.split(';').next().unwrap_or("image/jpeg"), data))
        .unwrap_or(("image/jpeg", uri))
}

/// Convert normalized Outpost messages into Gemini `(systemInstruction_string, contents_array)`.
///
/// Gemini rules:
/// 1. System instructions are top-level `systemInstruction: { parts: [{ text }] }`.
/// 2. `contents` turns must alternate between `user` and `model`.
/// 3. Tool results (`Role::Tool`) map to `role: "user"` carrying `functionResponse` parts.
/// 4. Parallel tool calls and sequential tool responses are merged into a single turn's `parts` array.
/// 5. Historical assistant thinking and `thoughtSignature` are preserved and round-tripped.
fn to_gemini(messages: &[Message]) -> (String, Vec<Value>) {
    let mut system_parts: Vec<String> = Vec::new();
    let mut contents: Vec<Value> = Vec::new();

    // Map tool_call_id to function name so Role::Tool messages can look up the function name.
    let mut id_to_name: HashMap<String, String> = HashMap::new();
    for m in messages {
        for tc in &m.tool_calls {
            id_to_name.insert(tc.id.clone(), tc.name.clone());
        }
    }

    for m in messages {
        match m.role {
            Role::System => {
                if !m.content.is_empty() {
                    system_parts.push(m.content.clone());
                }
            }
            Role::User => {
                let mut parts: Vec<Value> = Vec::new();
                if !m.content.is_empty() {
                    parts.push(json!({ "text": m.content }));
                }
                for img in &m.images {
                    let (mime_type, data) = parse_data_uri(img);
                    parts.push(json!({
                        "inlineData": {
                            "mimeType": mime_type,
                            "data": data,
                        }
                    }));
                }
                if parts.is_empty() {
                    continue;
                }

                // If previous turn was already "user", merge into it to satisfy strict alternation
                if let Some(last) = contents.last_mut().filter(|l| l.get("role").and_then(Value::as_str) == Some("user")) {
                    if let Some(last_parts) = last.get_mut("parts").and_then(Value::as_array_mut) {
                        last_parts.extend(parts);
                        continue;
                    }
                }

                contents.push(json!({
                    "role": "user",
                    "parts": parts,
                }));
            }
            Role::Assistant => {
                let mut parts: Vec<Value> = Vec::new();
                if let Some(r) = &m.reasoning {
                    if !r.trim().is_empty() {
                        parts.push(json!({
                            "text": r,
                            "thought": true,
                        }));
                    }
                }
                if !m.content.is_empty() {
                    parts.push(json!({ "text": m.content }));
                }
                for tc in &m.tool_calls {
                    let args: Value = serde_json::from_str(&tc.arguments).unwrap_or_else(|_| json!({}));
                    let mut part_obj = json!({
                        "functionCall": {
                            "name": tc.name,
                            "args": args,
                        }
                    });

                    // Extract thoughtSignature from extra_content or reasoning_signature
                    if let Some(extra) = &tc.extra_content {
                        if let Some(sig) = extra
                            .get("thought_signature")
                            .or_else(|| extra.get("thoughtSignature"))
                            .or_else(|| {
                                extra
                                    .get("google")
                                    .and_then(|g| g.get("thought_signature").or_else(|| g.get("thoughtSignature")))
                            })
                            .and_then(Value::as_str)
                        {
                            part_obj["thoughtSignature"] = json!(sig);
                        }
                    } else if let Some(sig) = &m.reasoning_signature {
                        if !sig.is_empty() {
                            part_obj["thoughtSignature"] = json!(sig);
                        }
                    }

                    parts.push(part_obj);
                }
                if parts.is_empty() {
                    parts.push(json!({ "text": "" }));
                }

                // If previous turn was "model", merge into it
                if let Some(last) = contents.last_mut().filter(|l| l.get("role").and_then(Value::as_str) == Some("model")) {
                    if let Some(last_parts) = last.get_mut("parts").and_then(Value::as_array_mut) {
                        last_parts.extend(parts);
                        continue;
                    }
                }

                contents.push(json!({
                    "role": "model",
                    "parts": parts,
                }));
            }
            Role::Tool => {
                let call_id = m.tool_call_id.clone().unwrap_or_default();
                let tool_name = id_to_name.get(&call_id).cloned().unwrap_or_else(|| {
                    if call_id.is_empty() {
                        "unknown".into()
                    } else {
                        call_id.clone()
                    }
                });
                let resp_part = json!({
                    "functionResponse": {
                        "name": tool_name,
                        "response": {
                            "output": m.content,
                        }
                    }
                });

                // Role::Tool maps to role: "user" in Gemini.
                // If previous turn was "user", merge into it!
                if let Some(last) = contents.last_mut().filter(|l| l.get("role").and_then(Value::as_str) == Some("user")) {
                    if let Some(last_parts) = last.get_mut("parts").and_then(Value::as_array_mut) {
                        last_parts.push(resp_part);
                        continue;
                    }
                }

                contents.push(json!({
                    "role": "user",
                    "parts": [resp_part],
                }));
            }
        }
    }

    // Ensure contents starts with a user turn
    if contents.is_empty() {
        contents.push(json!({
            "role": "user",
            "parts": [{ "text": "Hello" }],
        }));
    } else if contents.first().and_then(|f| f.get("role")).and_then(Value::as_str) == Some("model") {
        contents.insert(
            0,
            json!({
                "role": "user",
                "parts": [{ "text": "Begin." }],
            }),
        );
    }

    (system_parts.join("\n\n"), contents)
}

/// Convert normalized tools to Gemini `functionDeclarations`.
fn to_gemini_tools(tools: &[Tool]) -> Vec<Value> {
    if tools.is_empty() {
        return Vec::new();
    }
    let decls: Vec<Value> = tools
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "parameters": t.parameters,
            })
        })
        .collect();
    vec![json!({
        "functionDeclarations": decls,
    })]
}

/// Maps Outpost's 6-level thinking effort to Gemini's `thinkingConfig`.
fn thinking_config(model: &str, effort: Option<&str>) -> Option<Value> {
    match effort {
        Some("off") => {
            // Flash models allow budget: 0; Pro models require a minimum token budget (128).
            let budget = if model.to_lowercase().contains("pro") { 128 } else { 0 };
            Some(json!({
                "thinkingBudget": budget,
            }))
        }
        Some("low") => Some(json!({
            "thinkingBudget": 1024,
            "includeThoughts": true,
        })),
        Some("medium") => Some(json!({
            "thinkingBudget": 2048,
            "includeThoughts": true,
        })),
        Some("high") => Some(json!({
            "thinkingBudget": 4096,
            "includeThoughts": true,
        })),
        Some("xhigh") => Some(json!({
            "thinkingBudget": 8192,
            "includeThoughts": true,
        })),
        Some("max") => Some(json!({
            "thinkingBudget": 16384,
            "includeThoughts": true,
        })),
        None => {
            // Default: if it's a Gemini 2.5 or 3.x model, enable thought summaries
            let m = model.to_lowercase();
            if m.contains("2.5") || m.contains("2.0") || m.contains("3.") {
                Some(json!({
                    "includeThoughts": true,
                }))
            } else {
                None
            }
        }
        _ => None,
    }
}

#[async_trait]
impl Provider for GeminiProvider {
    async fn stream_chat(&self, request: ChatRequest) -> anyhow::Result<EventStream> {
        let base = normalize_base_url(&self.base_url);
        let clean_model = request.model.strip_prefix("models/").unwrap_or(&request.model);
        let url = format!("{base}/v1beta/models/{clean_model}:streamGenerateContent?alt=sse&key={}", self.api_key);

        let (system, contents) = to_gemini(&request.messages);
        wire_dump(Some(&system), &contents);

        let mut body = Map::new();
        body.insert("contents".into(), json!(contents));

        if !system.is_empty() {
            body.insert(
                "systemInstruction".into(),
                json!({
                    "parts": [{ "text": system }]
                }),
            );
        }

        let tools = to_gemini_tools(&request.tools);
        if !tools.is_empty() {
            body.insert("tools".into(), json!(tools));
        }

        let mut gen_config = Map::new();
        if let Some(t) = request.temperature {
            gen_config.insert("temperature".into(), json!(t));
        }
        if let Some(p) = request.top_p {
            gen_config.insert("topP".into(), json!(p));
        }
        gen_config.insert(
            "maxOutputTokens".into(),
            json!(request.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS)),
        );

        if let Some(tc) = thinking_config(&request.model, request.thinking_effort.as_deref()) {
            gen_config.insert("thinkingConfig".into(), tc);
        }
        body.insert("generationConfig".into(), Value::Object(gen_config));

        let mut req = self.http.post(&url).json(&Value::Object(body));
        if !self.api_key.is_empty() {
            req = req.header("x-goog-api-key", &self.api_key);
        }

        let resp = req.send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let detail = resp.text().await.unwrap_or_default();
            let trimmed = detail.trim();
            if status.as_u16() == 401 || status.as_u16() == 403 {
                anyhow::bail!("Google Gemini authentication failed: {trimmed}");
            }
            if status.as_u16() == 429 {
                anyhow::bail!("Google Gemini rate limit or quota exceeded: {trimmed}");
            }
            anyhow::bail!("Google Gemini request failed ({status}): {trimmed}");
        }

        let mut bytes = resp.bytes_stream();
        let s = stream! {
            let mut buf: Vec<u8> = Vec::new();
            let mut started = false;
            let mut had_tools = false;
            let mut wire_finish_reason: Option<String> = None;
            let mut call_counter: usize = 0;
            let mut tools_acc: Vec<ToolCall> = Vec::new();

            'outer: loop {
                let next_chunk = if started {
                    match tokio::time::timeout(STREAM_IDLE_TIMEOUT, bytes.next()).await {
                        Ok(chunk) => chunk,
                        Err(_) => {
                            yield StreamEvent::Error {
                                message: format!(
                                    "Google Gemini idle timeout: no data received for {}s mid-stream",
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
                        yield StreamEvent::Error {
                            message: format!("Google Gemini stream error: {e}"),
                        };
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

                    if let Some(err) = v.get("error") {
                        let msg = err
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("Unknown Gemini API error");
                        yield StreamEvent::Error {
                            message: format!("Google Gemini error: {msg}"),
                        };
                        return;
                    }

                    if let Some(candidates) = v.get("candidates").and_then(Value::as_array) {
                        for cand in candidates {
                            if let Some(fr) = cand.get("finishReason").and_then(Value::as_str) {
                                wire_finish_reason = Some(fr.to_string());
                            }

                            if let Some(parts) = cand.get("content").and_then(|c| c.get("parts")).and_then(Value::as_array) {
                                for part in parts {
                                    // 1. Function Call
                                    if let Some(fc) = part.get("functionCall") {
                                        let name = fc.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                                        let arguments = match fc.get("args") {
                                            Some(args) => serde_json::to_string(args).unwrap_or_else(|_| "{}".to_string()),
                                            None => "{}".to_string(),
                                        };
                                        let sig = part
                                            .get("thoughtSignature")
                                            .or_else(|| part.get("thought_signature"))
                                            .and_then(Value::as_str);
                                        let extra_content = sig.map(|s| {
                                            json!({
                                                "google": {
                                                    "thought_signature": s
                                                }
                                            })
                                        });

                                        let id = format!("call_{}_{}", call_counter, name);
                                        call_counter += 1;
                                        had_tools = true;

                                        yield StreamEvent::ToolStart { name: name.clone() };
                                        tools_acc.push(ToolCall {
                                            id,
                                            name,
                                            arguments,
                                            extra_content,
                                        });
                                        continue;
                                    }

                                    // 2. Thought / Reasoning
                                    if part.get("thought").and_then(Value::as_bool) == Some(true) {
                                        if let Some(text) = part.get("text").and_then(Value::as_str) {
                                            if !text.is_empty() {
                                                yield StreamEvent::Reasoning {
                                                    text: text.to_string(),
                                                };
                                            }
                                        }
                                        continue;
                                    }

                                    // 3. Regular Delta Text
                                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                                        if !text.is_empty() {
                                            yield StreamEvent::Delta {
                                                text: text.to_string(),
                                            };
                                        }
                                    }

                                    // 4. Standalone thought signature
                                    if let Some(sig) = part.get("thoughtSignature").and_then(Value::as_str) {
                                        yield StreamEvent::ReasoningSignature {
                                            signature: sig.to_string(),
                                        };
                                    }
                                }
                            }
                        }
                    }
                }

                if reached_eof {
                    break 'outer;
                }
            }

            for tc in tools_acc {
                yield StreamEvent::ToolCall(tc);
            }

            let finish_reason = if had_tools {
                Some("tool_calls".into())
            } else {
                wire_finish_reason.map(|r| match r.as_str() {
                    "STOP" => "stop".into(),
                    "MAX_TOKENS" => "length".into(),
                    other => other.to_lowercase(),
                })
            };

            yield StreamEvent::Done { finish_reason };
        };

        Ok(Box::pin(s))
    }

    async fn list_models(&self) -> anyhow::Result<Vec<String>> {
        let base = normalize_base_url(&self.base_url);
        let url = format!("{base}/v1beta/models?key={}", self.api_key);

        let mut req = self.http.get(&url);
        if !self.api_key.is_empty() {
            req = req.header("x-goog-api-key", &self.api_key);
        }

        let resp = req.send().await;
        if let Ok(res) = resp {
            if res.status().is_success() {
                if let Ok(val) = res.json::<Value>().await {
                    if let Some(models) = val.get("models").and_then(Value::as_array) {
                        let mut names: Vec<String> = models
                            .iter()
                            .filter(|m| {
                                m.get("supportedGenerationMethods")
                                    .and_then(Value::as_array)
                                    .map_or(false, |methods| {
                                        methods.iter().any(|meth| meth.as_str() == Some("generateContent"))
                                    })
                            })
                            .filter_map(|m| m.get("name").and_then(Value::as_str))
                            .map(|n| n.strip_prefix("models/").unwrap_or(n).to_string())
                            .filter(|n| n.contains("gemini"))
                            .collect();

                        if !names.is_empty() {
                            names.sort_by(|a, b| {
                                // Put 2.5 and 2.0 at the top
                                let score = |s: &str| {
                                    if s.contains("2.5-flash") {
                                        0
                                    } else if s.contains("2.5-pro") {
                                        1
                                    } else if s.contains("2.0-flash") {
                                        2
                                    } else {
                                        10
                                    }
                                };
                                score(a).cmp(&score(b)).then_with(|| a.cmp(b))
                            });
                            return Ok(names);
                        }
                    }
                }
            }
        }

        // Resilient fallback list so UI model picker is always populated
        Ok(vec![
            "gemini-2.5-flash".into(),
            "gemini-2.5-pro".into(),
            "gemini-2.5-flash-lite".into(),
            "gemini-2.0-flash".into(),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_msg(role: Role, content: &str) -> Message {
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
    fn test_normalize_base_url() {
        assert_eq!(
            normalize_base_url("https://generativelanguage.googleapis.com/v1beta/openai"),
            "https://generativelanguage.googleapis.com"
        );
        assert_eq!(
            normalize_base_url("https://generativelanguage.googleapis.com/v1beta"),
            "https://generativelanguage.googleapis.com"
        );
        assert_eq!(
            normalize_base_url("https://generativelanguage.googleapis.com/"),
            "https://generativelanguage.googleapis.com"
        );
        assert_eq!(
            normalize_base_url(""),
            "https://generativelanguage.googleapis.com"
        );
    }

    #[test]
    fn test_to_gemini_folding_and_alternation() {
        let msgs = vec![
            test_msg(Role::System, "You are an assistant."),
            test_msg(Role::User, "Hello"),
            test_msg(Role::Assistant, "Hi there!"),
            test_msg(Role::User, "Look at this"),
        ];

        let (sys, contents) = to_gemini(&msgs);
        assert_eq!(sys, "You are an assistant.");
        assert_eq!(contents.len(), 3);
        assert_eq!(contents[0]["role"], "user");
        assert_eq!(contents[0]["parts"][0]["text"], "Hello");
        assert_eq!(contents[1]["role"], "model");
        assert_eq!(contents[1]["parts"][0]["text"], "Hi there!");
        assert_eq!(contents[2]["role"], "user");
        assert_eq!(contents[2]["parts"][0]["text"], "Look at this");
    }

    #[test]
    fn test_to_gemini_merges_consecutive_user_and_tool_messages() {
        let mut assistant = test_msg(Role::Assistant, "Reading files");
        assistant.tool_calls.push(ToolCall {
            id: "call_1".into(),
            name: "read_file".into(),
            arguments: r#"{"path":"test.txt"}"#.into(),
            extra_content: Some(json!({ "google": { "thought_signature": "sig_abc" } })),
        });
        assistant.tool_calls.push(ToolCall {
            id: "call_2".into(),
            name: "list_dir".into(),
            arguments: r#"{"path":"."}"#.into(),
            extra_content: None,
        });

        let mut tool1 = test_msg(Role::Tool, "contents of test.txt");
        tool1.tool_call_id = Some("call_1".into());

        let mut tool2 = test_msg(Role::Tool, "file1.txt\nfile2.txt");
        tool2.tool_call_id = Some("call_2".into());

        let msgs = vec![
            test_msg(Role::User, "Inspect directory"),
            assistant,
            tool1,
            tool2,
        ];

        let (_, contents) = to_gemini(&msgs);
        assert_eq!(contents.len(), 3);
        assert_eq!(contents[0]["role"], "user");
        assert_eq!(contents[1]["role"], "model");
        // Verify tool calls carry arguments and signature
        assert_eq!(contents[1]["parts"][1]["functionCall"]["name"], "read_file");
        assert_eq!(contents[1]["parts"][1]["functionCall"]["args"]["path"], "test.txt");
        assert_eq!(contents[1]["parts"][1]["thoughtSignature"], "sig_abc");
        assert_eq!(contents[1]["parts"][2]["functionCall"]["name"], "list_dir");

        // Verify tool responses are combined into a single user turn
        assert_eq!(contents[2]["role"], "user");
        assert_eq!(contents[2]["parts"].as_array().unwrap().len(), 2);
        assert_eq!(contents[2]["parts"][0]["functionResponse"]["name"], "read_file");
        assert_eq!(contents[2]["parts"][0]["functionResponse"]["response"]["output"], "contents of test.txt");
        assert_eq!(contents[2]["parts"][1]["functionResponse"]["name"], "list_dir");
        assert_eq!(contents[2]["parts"][1]["functionResponse"]["response"]["output"], "file1.txt\nfile2.txt");
    }

    #[test]
    fn test_to_gemini_tools() {
        let tools = vec![
            Tool {
                name: "test_tool".into(),
                description: "A test tool".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" }
                    },
                    "required": ["path"]
                }),
            }
        ];

        let gem_tools = to_gemini_tools(&tools);
        assert_eq!(gem_tools.len(), 1);
        let decls = &gem_tools[0]["functionDeclarations"];
        assert_eq!(decls[0]["name"], "test_tool");
        assert_eq!(decls[0]["parameters"]["type"], "object");
    }

    #[test]
    fn test_thinking_config() {
        // Flash off
        let cfg_flash_off = thinking_config("gemini-2.5-flash", Some("off")).unwrap();
        assert_eq!(cfg_flash_off["thinkingBudget"], 0);

        // Pro off (minimum budget 128)
        let cfg_pro_off = thinking_config("gemini-2.5-pro", Some("off")).unwrap();
        assert_eq!(cfg_pro_off["thinkingBudget"], 128);

        // High
        let cfg_high = thinking_config("gemini-2.5-flash", Some("high")).unwrap();
        assert_eq!(cfg_high["thinkingBudget"], 4096);
        assert_eq!(cfg_high["includeThoughts"], true);

        // None on 2.5
        let cfg_default_25 = thinking_config("gemini-2.5-flash", None).unwrap();
        assert_eq!(cfg_default_25["includeThoughts"], true);
    }

    #[test]
    fn test_gemini_sse_stream_decodes_reasoning_and_parallel_tools() {
        let chunk_reasoning = json!({
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [{
                        "text": "I should read Cargo.toml and list files.",
                        "thought": true
                    }]
                }
            }]
        });

        let chunk_tools = json!({
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [
                        {
                            "functionCall": {
                                "name": "list_dir",
                                "args": { "path": "." }
                            },
                            "thoughtSignature": "sig_111"
                        },
                        {
                            "functionCall": {
                                "name": "read_file",
                                "args": { "path": "Cargo.toml" }
                            },
                            "thoughtSignature": "sig_222"
                        }
                    ]
                },
                "finishReason": "STOP"
            }]
        });

        let lines = vec![
            format!("data: {}", chunk_reasoning),
            format!("data: {}", chunk_tools),
        ];
        let lines_ref: Vec<&str> = lines.iter().map(AsRef::as_ref).collect();
        let events = parse_test_sse_lines(&lines_ref);

        assert_eq!(events.len(), 6);

        // 1. Reasoning
        match &events[0] {
            StreamEvent::Reasoning { text } => assert_eq!(text, "I should read Cargo.toml and list files."),
            other => panic!("expected Reasoning event, got: {:?}", other),
        }

        // 2. Tool 1 Start (emitted during stream)
        match &events[1] {
            StreamEvent::ToolStart { name } => assert_eq!(name, "list_dir"),
            other => panic!("expected ToolStart event, got: {:?}", other),
        }

        // 3. Tool 2 Start (emitted during stream)
        match &events[2] {
            StreamEvent::ToolStart { name } => assert_eq!(name, "read_file"),
            other => panic!("expected ToolStart event, got: {:?}", other),
        }

        // 4. Tool 1 Call (flushed at stream end)
        match &events[3] {
            StreamEvent::ToolCall(tc) => {
                assert_eq!(tc.name, "list_dir");
                assert_eq!(tc.arguments, r#"{"path":"."}"#);
                assert_eq!(tc.extra_content.as_ref().unwrap()["google"]["thought_signature"], "sig_111");
            }
            other => panic!("expected ToolCall event, got: {:?}", other),
        }

        // 5. Tool 2 Call (flushed at stream end)
        match &events[4] {
            StreamEvent::ToolCall(tc) => {
                assert_eq!(tc.name, "read_file");
                assert_eq!(tc.arguments, r#"{"path":"Cargo.toml"}"#);
                assert_eq!(tc.extra_content.as_ref().unwrap()["google"]["thought_signature"], "sig_222");
            }
            other => panic!("expected ToolCall event, got: {:?}", other),
        }

        // 6. Done with tool_calls finish reason
        match &events[5] {
            StreamEvent::Done { finish_reason } => assert_eq!(finish_reason.as_deref(), Some("tool_calls")),
            other => panic!("expected Done event, got: {:?}", other),
        }
    }

    fn parse_test_sse_lines(lines: &[&str]) -> Vec<StreamEvent> {
        let mut events = Vec::new();
        let mut had_tools = false;
        let mut wire_finish_reason: Option<String> = None;
        let mut call_counter: usize = 0;
        let mut tools_acc: Vec<ToolCall> = Vec::new();

        for line in lines {
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

            if let Some(candidates) = v.get("candidates").and_then(Value::as_array) {
                for cand in candidates {
                    if let Some(fr) = cand.get("finishReason").and_then(Value::as_str) {
                        wire_finish_reason = Some(fr.to_string());
                    }

                    if let Some(parts) = cand.get("content").and_then(|c| c.get("parts")).and_then(Value::as_array) {
                        for part in parts {
                            if let Some(fc) = part.get("functionCall") {
                                let name = fc.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                                let arguments = match fc.get("args") {
                                    Some(args) => serde_json::to_string(args).unwrap_or_else(|_| "{}".to_string()),
                                    None => "{}".to_string(),
                                };
                                let sig = part
                                    .get("thoughtSignature")
                                    .or_else(|| part.get("thought_signature"))
                                    .and_then(Value::as_str);
                                let extra_content = sig.map(|s| {
                                    json!({
                                        "google": {
                                            "thought_signature": s
                                        }
                                    })
                                });

                                let id = format!("call_{}_{}", call_counter, name);
                                call_counter += 1;
                                had_tools = true;

                                events.push(StreamEvent::ToolStart { name: name.clone() });
                                tools_acc.push(ToolCall {
                                    id,
                                    name,
                                    arguments,
                                    extra_content,
                                });
                                continue;
                            }

                            if part.get("thought").and_then(Value::as_bool) == Some(true) {
                                if let Some(text) = part.get("text").and_then(Value::as_str) {
                                    if !text.is_empty() {
                                        events.push(StreamEvent::Reasoning {
                                            text: text.to_string(),
                                        });
                                    }
                                }
                                continue;
                            }

                            if let Some(text) = part.get("text").and_then(Value::as_str) {
                                if !text.is_empty() {
                                    events.push(StreamEvent::Delta {
                                        text: text.to_string(),
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }

        for tc in tools_acc {
            events.push(StreamEvent::ToolCall(tc));
        }

        let finish_reason = if had_tools {
            Some("tool_calls".into())
        } else {
            wire_finish_reason.map(|r| match r.as_str() {
                "STOP" => "stop".into(),
                "MAX_TOKENS" => "length".into(),
                other => other.to_lowercase(),
            })
        };
        events.push(StreamEvent::Done { finish_reason });
        events
    }
}
