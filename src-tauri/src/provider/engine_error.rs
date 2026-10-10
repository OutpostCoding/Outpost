// ── surface: backend ──────────────────────────────────
// Engine-error taxonomy for OpenAI-compatible servers: which failures may retry/salvage, which must surface.
// Exports: EngineErrorKind, classify_engine_error, unwrap_error_message, CONTEXT_EXCEEDED_HINT · Related: provider/lmstudio.rs

//! Engine-error classification for the OpenAI-compatible adapter family
//! (LM Studio, llama.cpp server, and friends). Every engine failure arrives
//! wearing the same outer wrapper — "Engine protocol predict stream returned an
//! error: {…}" — so matching on the wrapper conflates fatal errors with
//! recoverable parser crashes. This module is the single contract: an error's
//! KIND decides what the recovery pipeline may do with it:
//!
//! - `ContextExceeded` — fatal, never retryable: surface the actionable hint,
//!   skip salvage and the non-streaming fallback entirely.
//! - `DiffParser` — llama.cpp's token diff parser crashed mid-tool-call; the
//!   error payload may still carry completed calls to salvage, and a
//!   non-streaming retry is a legitimate last resort.
//! - `Other` — surface as-is; no salvage, no retry.
//!
//! The keyword lists are pinned by a fixture corpus of real captured payloads
//! in `fixtures/engine_errors/` (see the tests). When a server rewords an
//! error: save the payload there with its `# expect:` tag, and only then
//! adjust the lists.

use serde_json::Value;

#[derive(Debug, PartialEq, Eq)]
pub enum EngineErrorKind {
    /// Context window limit reached: fatal, prompt + generation exceeds n_ctx.
    ContextExceeded(String),
    /// llama.cpp / LM Studio token diff parser crash on tool calls or interleaved commentary.
    DiffParser(String),
    /// Any other server/predict error.
    Other(String),
}

pub const CONTEXT_EXCEEDED_HINT: &str =
    "The conversation exceeded the model's loaded context window in LM Studio. \
     Because the context window must accommodate both prompt history and generation, \
     raise the Context Length setting in LM Studio (e.g. 16k, 32k, or 64k) or lower the model's max output tokens. \
     You can also use Outpost's Compact feature in the thread header to compact conversation history while preserving context.";

/// Unwraps nested JSON error payloads from LM Studio / llama.cpp into the innermost error message.
/// E.g. `{"error": "Engine protocol predict stream returned an error: {\"code\":500,\"message\":\"Context size has been exceeded.\",\"type\":\"server_error\"}"}`
/// unwraps to `"Context size has been exceeded."`.
pub fn unwrap_error_message(raw_err: &str) -> String {
    let trimmed = raw_err.trim();
    if let Ok(val) = serde_json::from_str::<Value>(trimmed) {
        if let Some(err_val) = val.get("error") {
            if let Some(s) = err_val.as_str() {
                if let Some(start) = s.find('{') {
                    if let Some(end) = s.rfind('}') {
                        if let Ok(inner) = serde_json::from_str::<Value>(&s[start..=end]) {
                            return inner
                                .get("message")
                                .and_then(Value::as_str)
                                .unwrap_or(s)
                                .to_string();
                        }
                    }
                }
                return s.to_string();
            } else if let Some(m) = err_val.get("message").and_then(Value::as_str) {
                if let Some(start) = m.find('{') {
                    if let Some(end) = m.rfind('}') {
                        if let Ok(inner) = serde_json::from_str::<Value>(&m[start..=end]) {
                            return inner
                                .get("message")
                                .and_then(Value::as_str)
                                .unwrap_or(m)
                                .to_string();
                        }
                    }
                }
                return m.to_string();
            }
        } else if let Some(m) = val.get("message").and_then(Value::as_str) {
            if let Some(start) = m.find('{') {
                if let Some(end) = m.rfind('}') {
                    if let Ok(inner) = serde_json::from_str::<Value>(&m[start..=end]) {
                        return inner
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or(m)
                            .to_string();
                    }
                }
            }
            return m.to_string();
        }
    }
    trimmed.to_string()
}

/// Classifies an LM Studio / llama.cpp engine error into its actionable category.
/// Fatal errors like `ContextExceeded` must never be funneled through diff-parser
/// salvage or non-streaming retry pipelines.
pub fn classify_engine_error(raw_err: &str) -> EngineErrorKind {
    let unwrapped = unwrap_error_message(raw_err);
    let lower = unwrapped.to_lowercase();
    let raw_lower = raw_err.to_lowercase();

    // 1. Context size exceeded (fatal, non-retryable)
    if lower.contains("context size has been exceeded")
        || lower.contains("context length exceeded")
        || lower.contains("maximum context length")
        || lower.contains("exceeded context window")
        || lower.contains("context window exceeded")
        || lower.contains("model context window exceeded")
        || lower.contains("token limit exceeded")
        || raw_lower.contains("context size has been exceeded")
        || raw_lower.contains("maximum context length")
    {
        return EngineErrorKind::ContextExceeded(unwrapped);
    }

    // 2. llama.cpp / LM Studio tool diff parser crash
    // Note: Do NOT match bare "predict stream" — that is just the outer wrapper
    // for all engine protocol failures.
    if lower.contains("invalid diff")
        || lower.contains("already finalized")
        || lower.contains("tool call delta")
        || lower.contains("returned more name content after arguments started")
        || lower.contains("now finding less tool calls")
        || raw_lower.contains("invalid diff")
        || raw_lower.contains("already finalized")
        || raw_lower.contains("tool call delta")
        || raw_lower.contains("returned more name content after arguments started")
        || raw_lower.contains("now finding less tool calls")
    {
        return EngineErrorKind::DiffParser(unwrapped);
    }

    EngineErrorKind::Other(unwrapped)
}

#[cfg(test)]
mod tests {
    use super::{classify_engine_error, EngineErrorKind};

    #[test]
    fn test_classify_engine_error() {
        // Context length exceeded (wrapped in JSON or bare message)
        assert!(matches!(
            classify_engine_error("Engine protocol predict stream returned an error: {\"code\":500,\"message\":\"Context size has been exceeded.\",\"type\":\"server_error\"}"),
            EngineErrorKind::ContextExceeded(_)
        ));
        assert!(matches!(
            classify_engine_error(r#"{"error":"Engine protocol predict stream returned an error: {\"code\":500,\"message\":\"Context size has been exceeded.\",\"type\":\"server_error\"}"}"#),
            EngineErrorKind::ContextExceeded(_)
        ));
        assert!(matches!(
            classify_engine_error("maximum context length exceeded"),
            EngineErrorKind::ContextExceeded(_)
        ));
        assert!(matches!(
            classify_engine_error("model context window exceeded"),
            EngineErrorKind::ContextExceeded(_)
        ));

        // Diff parser errors
        assert!(matches!(
            classify_engine_error("Engine protocol predict stream returned a tool call delta for already finalized tool call 0."),
            EngineErrorKind::DiffParser(_)
        ));
        assert!(matches!(
            classify_engine_error("Invalid diff: now finding less tool calls!"),
            EngineErrorKind::DiffParser(_)
        ));
        assert!(matches!(
            classify_engine_error(r#"{"error":"Engine protocol predict stream returned an error: {\"code\":500,\"message\":\"Invalid diff: now finding less tool calls!\"}"}"#),
            EngineErrorKind::DiffParser(_)
        ));
        assert!(matches!(
            classify_engine_error("Engine protocol tool call 0 returned more name content after arguments started."),
            EngineErrorKind::DiffParser(_)
        ));
        assert!(matches!(
            classify_engine_error("tool call delta received unexpectedly"),
            EngineErrorKind::DiffParser(_)
        ));

        // Generic errors
        assert!(matches!(
            classify_engine_error("connection refused: 127.0.0.1:1234"),
            EngineErrorKind::Other(_)
        ));
    }

    /// Fixture corpus: real captured engine-error payloads in
    /// `fixtures/engine_errors/*.txt`, each with `# expect:` (context_exceeded |
    /// diff_parser | other), `# source:` provenance, and optional `# notes:`
    /// header lines followed by the raw payload exactly as it reached us. This
    /// turns every debugging post-mortem into an executable regression: when a
    /// server rewords an error, drop the payload in, tag it, and only then
    /// touch the keyword lists above.
    #[test]
    fn test_engine_error_fixtures() {
        use std::fs;
        use std::path::PathBuf;

        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join("engine_errors");
        let mut count = 0;
        for entry in
            fs::read_dir(&dir).unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        {
            let path = entry.expect("unreadable fixture entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("txt") {
                continue;
            }
            let raw = fs::read_to_string(&path).expect("fixture unreadable");
            let mut expect = String::new();
            let mut payload_lines = Vec::new();
            let mut in_header = true;
            for line in raw.lines() {
                if in_header && line.starts_with('#') {
                    if let Some(rest) = line[1..].trim().strip_prefix("expect:") {
                        expect = rest.trim().to_string();
                    }
                    continue;
                }
                in_header = false;
                payload_lines.push(line);
            }
            let payload = payload_lines.join("\n").trim().to_string();
            assert!(
                !expect.is_empty(),
                "fixture {} has no `# expect:` tag",
                path.file_name().unwrap().to_string_lossy()
            );

            let tag = match classify_engine_error(&payload) {
                EngineErrorKind::ContextExceeded(_) => "context_exceeded",
                EngineErrorKind::DiffParser(_) => "diff_parser",
                EngineErrorKind::Other(_) => "other",
            };
            assert_eq!(
                tag, expect,
                "fixture {} classified as {tag}, expected {expect}",
                path.file_name().unwrap().to_string_lossy()
            );
            count += 1;
        }
        assert!(
            count >= 6,
            "expected at least 6 fixtures in fixtures/engine_errors, found {count}"
        );
    }
}
