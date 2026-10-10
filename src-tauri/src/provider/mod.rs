// ── surface: backend ──────────────────────────────────
// Provider abstraction — the loop talks only to this trait; hosts the shared message types.
// Exports: Provider trait + ChatRequest/StreamEvent/etc. · Related: anthropic.rs, engine_error.rs, lmstudio.rs, agent.rs

//! Provider abstraction (spec §2): the agent loop talks ONLY to
//! this interface and never knows which backend is live. Two implementations: the
//! OpenAI-compatible adapter (LM Studio + most hosted APIs) and the native
//! Anthropic Messages adapter — both behind this one trait.

pub mod anthropic;
pub mod engine_error;
pub mod lmstudio;

use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures_util::Stream;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(PartialEq)]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// A tool the model may call. `parameters` is a JSON Schema object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// A tool invocation emitted by the model. `arguments` is a JSON string (the
/// OpenAI convention); the executor parses it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// A search/fetch the model performed during a turn — surfaced under the reply
/// as a lightweight "sources" trail. UI metadata; never sent back to the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    pub kind: String,
    pub text: String,
    /// For read-only retrieval tools (web_search/web_fetch/search_notes): the
    /// snippets or excerpt the tool returned, shown in the expandable sources
    /// trail. Capped UI-side; never sent back to the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    /// Image attachments on a USER turn (base64 data URIs), for vision models. The
    /// OpenAI-compatible adapter sends them as `image_url` content parts; the
    /// Anthropic adapter sends them as base64 `image` blocks. Empty on non-image turns.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
    /// Set on assistant turns that call tools.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// Set on tool-result turns (role = Tool), linking back to the call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Creation time (epoch ms), for the UI timestamp. Never sent to the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<i64>,
    /// The model's thinking for an assistant turn, captured separately from
    /// `content` (an inline `<think>` block or an OpenAI `reasoning_content`
    /// delta). Rendered as a collapsible block; for Anthropic it (plus its
    /// signature, below) is replayed within a tool-use turn, else not sent back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    /// Anthropic only: the thinking block's signature, captured during streaming
    /// so the block can be replayed beside a `tool_use` on the next loop iteration
    /// (Anthropic validates it). Transient within a turn; not persisted by the UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_signature: Option<String>,
    /// Searches/fetches performed for this assistant turn (UI sources trail).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<Source>,
    /// UI-only ordered render timeline for an assistant turn: reasoning / text /
    /// tool segments, in the order they streamed, so the transcript interleaves
    /// them (like Claude Code) instead of collapsing into flat buckets. Opaque to
    /// the backend — persisted and round-tripped, but never sent to the model
    /// (`to_openai_messages` / `to_anthropic` read only content + tool_calls).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub segments: Option<serde_json::Value>,
}

/// A normalized chat request. Each backend translates this into its own wire
/// format (OpenAI chat-completions for LM Studio; Messages API for Anthropic).
#[derive(Debug, Clone, Deserialize)]
pub struct ChatRequest {
    /// Model id, chosen by the UI from [`Provider::list_models`].
    pub model: String,
    pub messages: Vec<Message>,
    /// Tools offered to the model. Empty = no tool ceremony in the prompt
    /// (spec §10a — the cheapest prompt). Empty until §11 step 3.
    #[serde(default)]
    pub tools: Vec<Tool>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub top_p: Option<f32>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// Anthropic thinking control from the UI's Thinking selector: "off" | "low" |
    /// "medium" | "high" | "xhigh" | "max". None → the provider default. The
    /// OpenAI-compatible adapter ignores it (local models bake thinking into the
    /// model / `<think>` tags); the Anthropic adapter maps it to `thinking` +
    /// `output_config.effort`.
    #[serde(default)]
    pub thinking_effort: Option<String>,
    /// Cap on the estimated context (tokens) sent to the model, to stay under a
    /// long-context pricing tier (most providers raise rates above 200k). None or 0
    /// = no cap. Enforced by dropping the oldest whole turns while keeping the system
    /// prompt + most recent turns; the index carries the
    /// thread the dropped raw turns held.
    #[serde(default)]
    pub max_context_tokens: Option<u32>,
    /// How many tool-call rounds one turn may take before the loop forces a
    /// wrap-up. None → [`crate::agent::DEFAULT_MAX_ITERATIONS`]. Clamped by the
    /// agent, so an out-of-range value from the UI is corrected, not obeyed.
    ///
    /// Local-only, like `max_context_tokens` above: never serialized to a
    /// provider — the adapters build their wire bodies field by field.
    #[serde(default)]
    pub max_tool_calls: Option<u32>,
    /// Which provider reasoning dialect to map `thinking_effort` onto in the OpenAI
    /// adapter: "deepseek" → `thinking: {type}` + `reasoning_effort` (GLM shares the
    /// format). None → send no reasoning params (a generic OpenAI server may reject
    /// them; local models bake thinking into `<think>`). The Anthropic adapter reads
    /// `thinking_effort` directly and ignores this.
    #[serde(default)]
    pub reasoning_style: Option<String>,
    /// Whether to enforce sequential single-tool execution per generation round.
    /// When true, the provider halts generation upon detecting a second tool call,
    /// returning only the first completed call to enforce a serial Observe -> Act loop.
    #[serde(default)]
    pub sequential_tool_calls: Option<bool>,
}

/// Normalized streaming events. Both backends are translated into this one shape
/// so the loop and UI never branch on which provider is live.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    /// A chunk of generated text.
    Delta { text: String },
    /// A chunk of the model's reasoning/thinking, surfaced separately from the
    /// answer (an inline `<think>` block or an OpenAI `reasoning_content` delta).
    Reasoning { text: String },
    /// Anthropic only: the signature of the just-finished thinking block. Consumed
    /// by the agent loop (stored on the assistant turn for tool-use replay); not
    /// forwarded to the UI.
    ReasoningSignature { signature: String },
    /// A tool call has begun — its name is now known, but its (buffered) arguments
    /// may still be generating. Lets the UI show a grounding status line during
    /// the otherwise-silent stretch while the call is being produced.
    ToolStart { name: String },
    /// The model finished a turn by requesting a tool call.
    ToolCall(ToolCall),
    /// A tool (e.g. md_write) is requesting approval before it commits. The UI
    /// shows the diff and replies via the `resolve_edit` command.
    ToolApproval {
        id: u64,
        tool: String,
        path: String,
        diff: String,
        /// An optional heads-up shown above the diff (e.g. overwriting a page).
        warning: Option<String>,
        /// run_command / process_start only: the command awaiting approval, with the
        /// whitelist patterns that would make it auto-approve next time. `None` for
        /// every other tool.
        command: Option<CommandApproval>,
    },
    /// The raw text a tool returned, surfaced to the UI for the sources trail —
    /// read-only tools show snippets/excerpts; writes ignore it. `id` matches the
    /// originating `ToolCall` so the UI attaches it to the right source.
    ToolResult {
        id: String,
        name: String,
        result: String,
    },
    /// An image a tool produced (a `data:…;base64,…` URI), so the UI can SHOW the
    /// user what the model was given to look at. The picture reaches the model by a
    /// different road — a synthetic user turn, since tool results are text-only (see
    /// agent.rs) — and without this event that road is invisible: you couldn't tell
    /// a good screenshot from a blank one. `id` matches the originating `ToolCall`.
    ToolImage { id: String, image: String },
    /// Run code in the webview's sandbox (Pyodide). The UI executes it and replies
    /// via the `resolve_code` command; `id` matches the awaiting run_python call.
    RunCode { id: u64, code: String },
    /// Generation finished cleanly.
    Done { finish_reason: Option<String> },
    /// A recoverable error surfaced mid-stream.
    Error { message: String },
}

/// A command awaiting approval (`run_command` / legacy `process_start`), plus the
/// whitelist patterns an "Always allow" click would persist to stop it prompting.
#[derive(Debug, Clone, Serialize)]
pub struct CommandApproval {
    /// The exact command string (the argv display form for argv calls).
    pub text: String,
    /// Leading token of every segment (shell form) or the program token (argv form).
    /// Empty when the command can never auto-approve — it fails a safety guard — so
    /// the UI renders a disabled "Always allow" rather than one that no-ops.
    pub remember: Vec<String>,
}

pub type EventStream = Pin<Box<dyn Stream<Item = StreamEvent> + Send>>;

#[async_trait]
pub trait Provider: Send + Sync {
    /// Stream a chat completion as normalized [`StreamEvent`]s. Streaming is the
    /// mandatory path (spec §0); non-streaming callers collect this stream.
    async fn stream_chat(&self, request: ChatRequest) -> anyhow::Result<EventStream>;

    /// List model ids the backend currently serves — powers the model picker and
    /// doubles as a reachability check.
    async fn list_models(&self) -> anyhow::Result<Vec<String>>;
}

/// Construct a provider for a connection. `kind` selects the wire format —
/// "anthropic" for the native Messages adapter, anything else for the
/// OpenAI-compatible adapter (LM Studio, Mistral, Gemini's OpenAI endpoint,
/// Mercury, OpenRouter, custom). `base_url` + optional `api_key` configure it.
/// Fold the shared 6-level Thinking control onto ONE provider's wire params.
///
/// The single source of truth for reasoning dialects. Each provider's shape
/// was established by testing against the live API, not from a general rule:
/// DeepSeek/GLM need a toggle AND an effort field, OpenAI-family take a bare
/// effort with no clean disable, OpenRouter normalizes its own object. A blanket
/// approach does not work, and a wrong shape is usually IGNORED rather than
/// rejected — so it fails silently.
///
/// `dialect: None` (a local server) returns nothing: local thinking is a
/// chat-template property, handled by the caller if it needs to force it.
/// The HTTP client every adapter uses.
///
/// `reqwest::Client::new()` carries no timeout at all. For a streaming chat that
/// is *almost* right — a long generation must never be cut off mid-answer, so a
/// total-request deadline would be wrong — but it also meant a peer that accepted
/// the connection and then stalled before sending headers parked the turn
/// forever, and the UI's `busy` flag gates thread switching, so the whole app was
/// stuck until a restart.
///
/// So: bound only the phase that should never be slow. `connect_timeout` covers
/// an unreachable or half-open peer. There is deliberately NO read timeout — the
/// gap before the first token is legitimately minutes on a large local model
/// doing prompt processing, and cutting that off would break the case this app
/// exists for. Stop is what covers the rest, which is why the initial call is
/// inside the cancellation select in `run_turn`.
pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(20))
        .tcp_keepalive(std::time::Duration::from_secs(15))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

pub fn reasoning_params(
    dialect: Option<&str>,
    effort: Option<&str>,
) -> serde_json::Map<String, serde_json::Value> {
    use serde_json::json;
    let mut body = serde_json::Map::new();
    match dialect {
        // DeepSeek / GLM: `thinking: {type}` + a two-level `reasoning_effort`.
        Some("deepseek") => match effort {
            Some("off") => {
                body.insert("thinking".into(), json!({ "type": "disabled" }));
            }
            e => {
                body.insert("thinking".into(), json!({ "type": "enabled" }));
                // Their docs map low–high → high, x-high/max → max.
                let re = if matches!(e, Some("max") | Some("xhigh")) {
                    "max"
                } else {
                    "high"
                };
                body.insert("reasoning_effort".into(), json!(re));
            }
        },
        // OpenAI / Gemini / Mistral: a bare top-level `reasoning_effort`. None of
        // them offer a clean disable, so "off" just omits it (provider default).
        Some("openai") => {
            let re = match effort {
                Some("low") => Some("low"),
                Some("medium") => Some("medium"),
                Some("high") | Some("xhigh") | Some("max") => Some("high"),
                _ => None,
            };
            if let Some(re) = re {
                body.insert("reasoning_effort".into(), json!(re));
            }
        }
        // OpenRouter: its unified `reasoning: {effort}` object, normalized across
        // whatever model is routed to (off → "none").
        Some("openrouter") => {
            // Fold the six-level control into the three OpenRouter defines. This
            // was the only arm passing the raw level straight through, so `xhigh`
            // and `max` went on the wire as-is — and an unrecognised parameter is
            // IGNORED, not rejected, so choosing "max" silently got the model's
            // default instead of its deepest setting.
            let re = match effort.map(str::trim) {
                Some("off") => Some("none"),
                Some("low") => Some("low"),
                Some("medium") => Some("medium"),
                Some("high") | Some("xhigh") | Some("max") => Some("high"),
                _ => None,
            };
            if let Some(re) = re {
                body.insert("reasoning".into(), json!({ "effort": re }));
            }
        }
        // Inception AI (Mercury): `reasoning_effort` ("low", "medium", "high")
        // PLUS `reasoning_summary: true` so the API actually emits the thinking summary stream.
        Some("inception") => match effort {
            Some("off") => {}
            e => {
                body.insert("reasoning_summary".into(), json!(true));
                let re = match e {
                    Some("low") => "low",
                    Some("medium") => "medium",
                    Some("high") | Some("xhigh") | Some("max") => "high",
                    _ => "medium",
                };
                body.insert("reasoning_effort".into(), json!(re));
            }
        },
        _ => {}
    }
    body
}

#[cfg(test)]
mod reasoning_tests {
    use super::reasoning_params;

    fn p(dialect: Option<&str>, effort: &str) -> String {
        serde_json::to_string(&serde_json::Value::Object(reasoning_params(dialect, Some(effort))))
            .unwrap()
    }

    /// Every dialect must FOLD the six levels into what it actually accepts. A
    /// value a provider doesn't recognise is ignored rather than rejected, so
    /// getting this wrong is silent — the user picks max and gets the default.
    #[test]
    fn every_dialect_folds_all_six_levels() {
        for e in ["off", "low", "medium", "high", "xhigh", "max"] {
            // A source with no dialect (local LM Studio) is sent NOTHING.
            assert_eq!(p(None, e), "{}", "local should send nothing at {e}");

            let or = p(Some("openrouter"), e);
            for bad in ["xhigh", "max"] {
                assert!(!or.contains(bad), "openrouter leaked a raw level at {e}: {or}");
            }
            let oa = p(Some("openai"), e);
            for bad in ["xhigh", "max"] {
                assert!(!oa.contains(bad), "openai leaked a raw level at {e}: {oa}");
            }
            let inc = p(Some("inception"), e);
            for bad in ["xhigh", "max"] {
                assert!(!inc.contains(bad), "inception leaked a raw level at {e}: {inc}");
            }
        }
        // The one setting each provider spells differently.
        assert!(p(Some("openrouter"), "off").contains("none"));
        assert!(p(Some("deepseek"), "off").contains("disabled"));
        assert_eq!(p(Some("openai"), "off"), "{}"); // no clean disable → omit
        assert_eq!(p(Some("inception"), "off"), "{}"); // off → omit
        assert!(p(Some("inception"), "high").contains("reasoning_summary"));
        // DeepSeek is the exception: it HAS a "max" and should reach it.
        assert!(p(Some("deepseek"), "max").contains("max"));
    }
}

#[cfg(test)]
mod wire_dump_tests {
    use super::turn_preview;
    use serde_json::json;

    /// A plain-string turn previews its content.
    #[test]
    fn string_content_previews() {
        assert_eq!(
            turn_preview(&json!({ "content": "hello world" })),
            "hello world"
        );
    }

    /// A content-parts turn (vision) previews its text part and names the other
    /// block types, so an image-heavy turn stays recognisable in the dump.
    #[test]
    fn parts_content_previews_text_and_blocks() {
        let m = json!({
            "content": [
                { "type": "text", "text": "look at this" },
                { "type": "image_url", "image_url": {} }
            ]
        });
        assert_eq!(turn_preview(&m), "look at this [blocks: text, image_url]");
    }

    /// A turn with no content field still dumps something identifiable.
    #[test]
    fn missing_content_is_flagged() {
        assert_eq!(turn_preview(&json!({ "role": "user" })), "(no content)");
    }
}

/// A short, newline-free preview of a string for the wire dump.
fn preview(s: &str) -> String {
    s.chars().take(60).collect::<String>().replace(['\n', '\r'], " ")
}

/// The text preview of one SERIALIZED wire turn: plain-string content, the text
/// part of a content-parts array (vision), or a block-type summary for
/// Anthropic turns. Pure so it can be unit-tested independently of the dump's
/// output format.
fn turn_preview(m: &serde_json::Value) -> String {
    match m.get("content") {
        Some(serde_json::Value::String(s)) => preview(s),
        Some(serde_json::Value::Array(parts)) => {
            let text: String = parts
                .iter()
                .filter_map(|p| {
                    (p["type"].as_str() == Some("text"))
                        .then(|| p["text"].as_str().unwrap_or(""))
                })
                .collect();
            let kinds: Vec<&str> = parts
                .iter()
                .filter_map(|p| p["type"].as_str())
                .collect();
            let blocks = format!("[blocks: {}]", kinds.join(", "));
            if text.is_empty() {
                blocks
            } else {
                format!("{} {blocks}", preview(&text))
            }
        }
        _ => "(no content)".to_string(),
    }
}

/// Debug aid (investigation): dump the FINAL serialized wire — the folded
/// system prefix (Anthropic's top-level `system`, or the OpenAI-compatible
/// leading system row) plus each turn of the messages array — right BEFORE the
/// HTTP request goes out, so the ACTUAL wire can be inspected instead of
/// relying on model self-reports (which conflate role and position). Called
/// after `to_anthropic` / `to_openai_messages`, so the fold is already applied.
/// Gated behind `OUTPOST_WIRE_DUMP`; off by default, so it costs nothing when
/// unset.
pub fn wire_dump(system_prefix: Option<&str>, turns: &[serde_json::Value]) {
    if std::env::var_os("OUTPOST_WIRE_DUMP").is_none() {
        return;
    }
    if let Some(sys) = system_prefix {
        if !sys.is_empty() {
            eprintln!(
                "[wire-dump] system prefix ({:>4} chars) | {}",
                sys.chars().count(),
                preview(sys)
            );
        }
    }
    for (i, m) in turns.iter().enumerate() {
        let role = m.get("role").and_then(serde_json::Value::as_str).unwrap_or("?");
        eprintln!("[wire-dump] #{i:>3} {:<9} | {}", role, turn_preview(m));
    }
}

/// Construct a provider for a connection. `kind` selects the wire format —
/// "anthropic" for the native Messages adapter, anything else for the
/// OpenAI-compatible adapter (LM Studio, Mistral, Gemini's OpenAI endpoint,
/// Mercury, OpenRouter, custom). `base_url` + optional `api_key` configure it.
/// `mid_system_tail` is OBSOLETE (2026-08-30): the per-turn tail folds into
/// the single leading system block on every adapter and every model, so the
/// in-array capability flag is no longer consulted anywhere. Kept so the
/// frontend's `midSystemMessages` contract is unchanged — retire it with the
/// UI flag.
pub fn make_provider(
    kind: &str,
    base_url: &str,
    api_key: Option<String>,
    mid_system_tail: bool,
) -> Arc<dyn Provider> {
    match kind {
        "anthropic" => Arc::new(anthropic::AnthropicProvider::new(
            base_url,
            api_key.unwrap_or_default(),
            mid_system_tail,
        )),
        _ => Arc::new(lmstudio::LmStudioProvider::new(base_url, api_key)),
    }
}
