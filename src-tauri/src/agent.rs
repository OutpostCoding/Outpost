// ── surface: backend ──────────────────────────────────
// The agent loop — one streaming inference, escalating to a loop only on tool calls;
// holds cancellation (Stop) + the per-turn injection points for head/tail prompt parts.
// Exports: run_turn, DEFAULT_MAX_ITERATIONS · Related: commands.rs, provider/mod.rs, tools/mod.rs

//! The agent loop (spec §3). The default path is a single streaming inference
//! call. It escalates — looping — only when the model emits a tool call: run the
//! tool, feed the result back, continue. A cancellation token (Stop, §10) lets
//! the user interrupt mid-stream. No multi-agent / parallel machinery (§3).

use std::sync::Arc;

use futures_util::StreamExt;
use tauri::ipc::Channel;
use tokio_util::sync::CancellationToken;

use crate::provider::{ChatRequest, Message, Provider, Role, StreamEvent, ToolCall};
use crate::tools::Tools;

/// Safety bound on tool-call iterations. A real coding turn (read a few files,
/// make several edits, run tests, recover from a stumble) routinely needs a couple
/// dozen tool calls, so this is generous — it's a runaway backstop, not a budget.
/// The user can Stop at any time, and the model normally ends well before this.
///
/// Overridable per turn from Settings → Coding (`ChatRequest::max_tool_calls`);
/// this is the value used when the UI sends nothing.
pub const DEFAULT_MAX_ITERATIONS: usize = 40;

/// The ceiling stays a ceiling. The setting exists because 40 is sometimes too
/// few, not because the backstop is unwanted — an unbounded loop against a paid
/// endpoint is a different kind of mistake from a truncated turn, and only one of
/// them is recoverable by sending another message.
pub const MAX_ITERATIONS_LIMIT: usize = 200;

/// Resolve the UI's requested cap into a usable one. Out-of-range is clamped
/// rather than rejected: this arrives from a text field, and a turn refusing to
/// start because a number is too big is worse than a turn quietly using 200.
fn resolve_max_iterations(requested: Option<u32>) -> usize {
    match requested {
        Some(n) if n > 0 => (n as usize).min(MAX_ITERATIONS_LIMIT),
        _ => DEFAULT_MAX_ITERATIONS,
    }
}

/// How many times to silently re-request after a *degenerate* response (no tool
/// calls AND no text) before giving up and telling the user. Transient empties on
/// free / rate-limited endpoints usually clear on one retry; a model that stops
/// dead won't, so keep this small.
const MAX_EMPTY_RETRIES: usize = 1;

/// Did the provider stop because it ran out of output tokens?
///
/// Two spellings for one condition: OpenAI-compatible providers say `length`,
/// Anthropic says `max_tokens`. Testing only the first meant every truncated
/// Anthropic reply was reported as a normal completion — and Anthropic is where
/// it bites hardest, because adaptive thinking spends the SAME budget as the
/// answer, so a hard question can think its way to the cap.
fn is_truncated(finish: Option<&str>) -> bool {
    matches!(finish, Some("length") | Some("max_tokens"))
}

pub async fn run_turn(
    provider: Arc<dyn Provider>,
    tools: Arc<Tools>,
    mut request: ChatRequest,
    enabled_tools: Vec<String>,
    vision_on: bool,
    context_files: Vec<String>,
    project_root: Option<std::path::PathBuf>,
    cancel: CancellationToken,
    on_event: &Channel<StreamEvent>,
) -> anyhow::Result<()> {
    request.tools = tools
        .definitions()
        .into_iter()
        .filter(|t| enabled_tools.iter().any(|e| e == &t.name))
        .collect();

    let mut seen_paths: Vec<String> = Vec::new();
    let mut empty_retries = 0usize;
    let mut last_tool_call: Option<(String, String, usize)> = None;
    let max_iterations = resolve_max_iterations(request.max_tool_calls);

    for iteration_index in 0..max_iterations {
        if cancel.is_cancelled() {
            let _ = on_event.send(StreamEvent::Done {
                finish_reason: Some("cancelled".into()),
            });
            return Ok(());
        }

        let mut sent = request.clone();
        if let Some(budget) = request.max_context_tokens {
            if budget > 0 {
                sent.messages = cap_context(&sent.messages, budget as usize);
            }
        }

        let change = tools.context_change(project_root.as_deref());
        let tail = tools.tail_context(
            project_root.as_deref(),
            &context_files,
            iteration_index == 0,
        );
        
        append_tail(
            &mut sent.messages,
            &[
                change.as_deref(),
                tail.as_deref(),
            ],
        );
        
        let mut stream = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                let _ = on_event.send(StreamEvent::Done {
                    finish_reason: Some("cancelled".into()),
                });
                return Ok(());
            }
            opened = provider.stream_chat(sent) => opened?,
        };
        let mut assistant_text = String::new();
        let mut assistant_reasoning = String::new();
        let mut assistant_signature: Option<String> = None;
        let mut calls: Vec<ToolCall> = Vec::new();
        let mut cancelled = false;
        let mut last_finish: Option<String> = None;

        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    cancelled = true;
                    break;
                }
                maybe_event = stream.next() => {
                    let Some(event) = maybe_event else { break };
                    match event {
                        StreamEvent::Delta { text } => {
                            assistant_text.push_str(&text);
                            if on_event.send(StreamEvent::Delta { text }).is_err() {
                                return Ok(()); // webview dropped the channel
                            }
                        }
                        StreamEvent::Reasoning { text } => {
                            assistant_reasoning.push_str(&text);
                            if on_event.send(StreamEvent::Reasoning { text }).is_err() {
                                return Ok(());
                            }
                        }
                        // Backend-internal: keep the thinking block's signature so it
                        // can be replayed beside a tool_use next iteration (Anthropic).
                        // Not forwarded to the UI.
                        StreamEvent::ReasoningSignature { signature } => {
                            assistant_signature = Some(signature);
                        }
                        StreamEvent::ToolStart { name } => {
                            let _ = on_event.send(StreamEvent::ToolStart { name });
                        }
                        StreamEvent::ToolCall(call) => {
                            eprintln!(
                                "[agent] tool_call: {} ({} arg bytes)",
                                call.name,
                                call.arguments.len()
                            );
                            let _ = on_event.send(StreamEvent::ToolCall(call.clone()));
                            calls.push(call);
                        }
                        // Approval/result/run-code events originate here, not from the
                        // provider.
                        StreamEvent::ToolApproval { .. }
                        | StreamEvent::ToolResult { .. }
                        | StreamEvent::ToolImage { .. }
                        | StreamEvent::RunCode { .. } => {}
                        StreamEvent::Error { message } => {
                            let _ = on_event.send(StreamEvent::Error { message });
                            return Ok(());
                        }
                        StreamEvent::Done { finish_reason } => {
                            last_finish = finish_reason;
                            break;
                        }
                    }
                }
            }
        }

        // Dropping `stream` here aborts the in-flight HTTP request on cancel.
        if cancelled {
            let _ = on_event.send(StreamEvent::Done {
                finish_reason: Some("cancelled".into()),
            });
            return Ok(());
        }

        // Degenerate response: no tool calls AND no visible answer. The model
        // returned nothing actionable — common on free / rate-limited endpoints, or
        // a reasoning model that "thinks" and then stops without answering. NEVER end
        // the turn silently on this (that's the "it just terminated" bug). Retry once
        // — a transient empty (especially on a free tier) usually clears on a plain
        // re-request; don't record the empty turn, so the retry re-sends the same
        // request rather than continuing past a blank assistant turn. If it's still
        // empty, say what happened instead of vanishing.
        if calls.is_empty() && assistant_text.trim().is_empty() {
            if empty_retries < MAX_EMPTY_RETRIES {
                empty_retries += 1;
                continue;
            }
            let detail = if is_truncated(last_finish.as_deref()) {
                "the model hit its output-token limit before answering — raise max_tokens \
                 (or the model's output cap), or try a different model."
            } else {
                "the model returned an empty response — no text and no tool call. This is \
                 common on free or rate-limited API endpoints, and with some reasoning \
                 models that stop after thinking. Try resending, or switch models."
            };
            let _ = on_event.send(StreamEvent::Delta {
                text: format!("\n⚠ The turn ended early: {detail}"),
            });
            let _ = on_event.send(StreamEvent::Done {
                finish_reason: Some("empty".into()),
            });
            return Ok(());
        }
        empty_retries = 0;

        let trimmed_reasoning = assistant_reasoning.trim();
        let is_verbatim_duplicate = !trimmed_reasoning.is_empty()
            && trimmed_reasoning.len() >= 30
            && assistant_signature.is_none()
            && request
                .messages
                .iter()
                .rev()
                .find(|m| m.role == Role::Assistant)
                .and_then(|m| m.reasoning.as_deref())
                .map_or(false, |r| r.trim() == trimmed_reasoning);

        let reasoning_to_record = if is_verbatim_duplicate {
            eprintln!(
                "[agent] suppressing verbatim duplicate reasoning ({} bytes) in request.messages to prevent induction loop",
                trimmed_reasoning.len()
            );
            None
        } else if assistant_reasoning.is_empty() {
            None
        } else {
            Some(assistant_reasoning)
        };

        // Record the assistant turn (its text plus any tool calls it requested).
        request.messages.push(Message {
            role: Role::Assistant,
            content: assistant_text,
            images: Vec::new(),
            tool_calls: calls.clone(),
            tool_call_id: None,
            created_at: None,
            reasoning: reasoning_to_record,
            reasoning_signature: assistant_signature,
            sources: Vec::new(),
            segments: None,
        });

        if calls.is_empty() {
            // A cut-off answer must not be presented as a finished one. The
            // empty-response branch above already warns when nothing came back;
            // this is the same failure with SOME text, which is the harder case
            // to notice — it reads as a complete reply that simply stops.
            if is_truncated(last_finish.as_deref()) {
                let _ = on_event.send(StreamEvent::Delta {
                    text: "\n\n⚠ This reply was cut off at the output-token limit — raise \
                           max_tokens for this connection, or ask for the rest."
                        .into(),
                });
            }
            let _ = on_event.send(StreamEvent::Done {
                finish_reason: Some("stop".into()),
            });
            return Ok(());
        }

        // Images produced by this round's tools, collected across the calls below
        // and handed to the model in ONE synthetic user turn after the loop.
        let mut shots: Vec<String> = Vec::new();

        // Run each requested tool, feed the result back, then loop.
        for call in calls {
            // Stop has to actually stop. The stream loop above breaks on cancel,
            // but a round can arrive holding several calls, and without this
            // every remaining one still ran: writing to disk unattended under
            // `auto_project_writes`, or raising an approval overlay AFTER the
            // user asked to stop — which the turn then blocks on indefinitely,
            // because `Approvals` is a bare oneshot with no cancellation.
            if cancel.is_cancelled() {
                let _ = on_event.send(StreamEvent::Done {
                    finish_reason: Some("cancelled".into()),
                });
                return Ok(());
            }
            eprintln!("[agent] executing tool: {}", call.name);

            // Check for consecutive identical tool calls to prevent autoregressive looping
            let is_looping_call = match &mut last_tool_call {
                Some((last_name, last_args, count)) if last_name == &call.name && last_args == &call.arguments => {
                    *count += 1;
                    *count >= 2
                }
                _ => {
                    last_tool_call = Some((call.name.clone(), call.arguments.clone(), 0));
                    false
                }
            };
            let result = if is_looping_call {
                let count = last_tool_call.as_ref().map(|(_, _, c)| *c).unwrap_or(2) + 1;
                eprintln!(
                    "[agent] halting consecutive identical call to '{}' (attempt #{})",
                    call.name, count
                );
                format!(
                    "Error: Repeated identical call to '{}' with identical arguments detected (attempted {} times consecutively). \
                     This operation has already been executed without changing state or outcome. \
                     Do not repeat the exact same call. Stop and reconsider your plan: inspect the actual file contents, test results, or error messages and try a different approach, or conclude your response.",
                    call.name, count
                )
            } else {
                // Stop has to interrupt DURING a tool, not just between tools: the
                // per-call check above only catches a cancel that landed before this
                // call started. Without this select, a long tool (a cold code_search
                // embed, a hanging command) holds the whole turn open after Stop —
                // the loop cannot observe the token again until the tool returns.
                // `biased` prefers the cancel branch the moment both are ready.
                // Dropping the tool future cancels it at its next await point —
                // cooperative by design: sync work inside a single poll runs to
                // completion, and detached `spawn_blocking` work is stopped by the
                // tool's OWN cancel checks (code_search's embed-batch loop), not by
                // this drop.
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {
                        let _ = on_event.send(StreamEvent::Done {
                            finish_reason: Some("cancelled".into()),
                        });
                        return Ok(());
                    }
                    res = tools.execute(
                        &call,
                        &enabled_tools,
                        vision_on,
                        &context_files,
                        &seen_paths,
                        project_root.as_deref(),
                        &cancel,
                        on_event,
                    ) => res,
                }
            };
            eprintln!(
                "[agent] tool {} returned ({} bytes)",
                call.name,
                result.len()
            );
            // Track files this turn has read or written, for html_write's collision
            // check (an intended overwrite vs. an accidental same-name new page).
            if matches!(call.name.as_str(), "read_note" | "html_write" | "html_edit") {
                if let Some(p) = serde_json::from_str::<serde_json::Value>(&call.arguments)
                    .ok()
                    .and_then(|v| v.get("path").and_then(|p| p.as_str()).map(str::to_string))
                {
                    if !seen_paths.contains(&p) {
                        seen_paths.push(p);
                    }
                }
            }
            // Surface the raw result to the UI for the sources trail (read-only
            // tools show snippets/excerpts; writes are already shown via the diff).
            let _ = on_event.send(StreamEvent::ToolResult {
                id: call.id.clone(),
                name: call.name.clone(),
                result: result.clone(),
            });
            // Drain per call, BEFORE call.id is moved below, so each image is tied
            // to the tool that produced it — the UI hangs the thumbnail on that
            // chip, and the user can see exactly what the model was handed.
            for image in tools.take_pending_images() {
                let _ = on_event.send(StreamEvent::ToolImage {
                    id: call.id.clone(),
                    image: image.clone(),
                });
                shots.push(image);
            }
            request.messages.push(Message {
                role: Role::Tool,
                content: result,
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: Some(call.id),
                created_at: None,
                reasoning: None,
                reasoning_signature: None,
                sources: Vec::new(),
                segments: None,
            });
        }

        // Hand over any images the tools produced. A tool result is TEXT-ONLY on
        // every provider, so an image can't ride back inside it; instead it goes in
        // a synthetic USER turn placed right after the results — the one shape every
        // vision model accepts. OpenAI-compatible backends see a `user` message of
        // image_url parts; Anthropic merges it into the same user turn as the
        // tool_results (to_anthropic's Role::User arm), because two consecutive user
        // turns are rejected there.
        //
        // The label is the TURN'S OWN TEXT, deliberately not a system message: a
        // mid-thread system turn is hoisted into Anthropic's top-level `system`, so
        // it would arrive detached from the image it describes.
        if !shots.is_empty() {
            let plural = if shots.len() == 1 { "" } else { "s" };
            request.messages.push(Message {
                role: Role::User,
                content: format!(
                    "Screenshot{plural} from the tool call above — the current visual state."
                ),
                images: shots,
                tool_calls: Vec::new(),
                tool_call_id: None,
                created_at: None,
                reasoning: None,
                reasoning_signature: None,
                sources: Vec::new(),
                segments: None,
            });
        }
    }

    // Hit the iteration ceiling without the model ending on its own. Don't cut off
    // silently mid-task — that's the "why did it just stop?" failure: the turn
    // vanishes with no summary and the last action half-finished. Give it one final
    // pass with NO tools offered, so it can't keep looping and instead wraps up —
    // with the full context in hand and nothing to call, it produces a closing
    // summary of where it got to. Skipped on cancel.
    if !cancel.is_cancelled() {
        // Nudge it to summarize by folding a note into the last tool result rather
        // than appending a new message — the note explains why the loop ended, and
        // folding keeps it adjacent to the result that ended it. (The old reason
        // was user/assistant alternation on the Anthropic backend; system
        // messages have since made that constraint moot, but the adjacency still
        // reads better.)
        if let Some(last) = request.messages.last_mut() {
            if matches!(last.role, Role::Tool) {
                last.content.push_str(
                    "\n\n[System: the tool-call budget for this turn is exhausted. \
                     This is a hard per-turn cap, not a comment on the task or the \
                     work's quality. No further tool calls will be executed this \
                     turn. Summarize for the user what you got done, what's verified, \
                     and what's left to pick up next.]",
                );
            }
        }
        // Keep the tool SCHEMAS. Clearing them reads as obviously right — offer
        // nothing, so it can't call anything — but this history necessarily ends
        // in a tool_use/tool_result pair, since that is the only way out of the
        // loop above, and Anthropic rejects a request whose messages reference
        // tools the request doesn't declare. So the wrap-up 400'd every single
        // time on that backend, and the error was swallowed twice on the way out,
        // producing exactly the silent stop this whole block exists to prevent.
        // The system note folded into the last tool result is what suppresses
        // further calls; any it emits anyway are simply not executed below.
        if let Some(budget) = request.max_context_tokens {
            if budget > 0 {
                request.messages = cap_context(&request.messages, budget as usize);
            }
        }
        // The wrap-up summary should see pinned documents too
        // — "what's left" often lives there — plus the pin change-note if one
        // happened this turn. No loop risk here: no tools are offered at the
        // wrap-up, so the model can't act on them.
        let change = tools.context_change(project_root.as_deref());
        let tail = tools.tail_context(project_root.as_deref(), &context_files, true);
        append_tail(
            &mut request.messages,
            &[change.as_deref(), tail.as_deref()],
        );
        // Whether the wrap-up actually said anything. If it didn't, the user is
        // back to a turn that stops with no explanation — so say it ourselves
        // rather than let the failure be silent a second time.
        let mut summarized = false;
        match provider.stream_chat(request).await {
            Ok(mut stream) => {
                while let Some(event) = stream.next().await {
                    match event {
                        StreamEvent::Delta { text } => {
                            summarized = true;
                            if on_event.send(StreamEvent::Delta { text }).is_err() {
                                break;
                            }
                        }
                        StreamEvent::Reasoning { text } => {
                            let _ = on_event.send(StreamEvent::Reasoning { text });
                        }
                        // Forward it. Discarding the error here is what turned a
                        // failed wrap-up into "it just stopped".
                        StreamEvent::Error { message } => {
                            let _ = on_event.send(StreamEvent::Error { message });
                            break;
                        }
                        StreamEvent::Done { .. } => break,
                        _ => {}
                    }
                }
            }
            Err(e) => {
                let _ = on_event.send(StreamEvent::Error {
                    message: format!("couldn't ask for a closing summary: {e}"),
                });
            }
        }
        if !summarized {
            let _ = on_event.send(StreamEvent::Delta {
                text: format!(
                    "\n\n⚠ This turn reached its limit of {max_iterations} tool calls and \
                     stopped. The work so far is done and on disk — send another message to \
                     carry on from here."
                ),
            });
        }
    }

    let _ = on_event.send(StreamEvent::Done {
        finish_reason: Some("max_iterations".into()),
    });
    Ok(())
}

/// Append the per-turn tail as one or more SYSTEM messages, in a fixed order:
/// the pin/unpin change-note, then pinned documents. Empty parts are skipped.
///
/// Those two are all that remain: approval posture, tool log, assembled-context
/// line and the current date+time were removed rather than re-gated — each was
/// volatile enough to bust the cached prefix on most turns (see NOTES.md
/// "Head/tail discipline"). Don't reintroduce them. Pinned docs ride iteration 0
/// and the wrap-up only (the `bb32f8c` loop lesson); the change-note is stable
/// once computed per turn.
///
/// Each part still rides as its OWN message so the parts can be capped and
/// gated independently — but on the WIRE every system message (the head, a
/// compaction handover note, and these tail parts) folds into ONE leading
/// system block before the transcript (provider/lmstudio.rs
/// `to_openai_messages`, provider/anthropic.rs `to_anthropic`). The model sees
/// a single system block at the start of context, never a system turn after
/// the transcript (2026-08-30 — see NOTES.md "single system block").
fn append_tail(messages: &mut Vec<Message>, parts: &[Option<&str>]) {
    if messages.is_empty() {
        return;
    }
    for part in parts {
        let Some(text) = part else { continue };
        if text.trim().is_empty() {
            continue;
        }
        messages.push(Message {
            role: Role::System,
            content: text.to_string(),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            created_at: None,
            reasoning: None,
            reasoning_signature: None,
            sources: Vec::new(),
            segments: None,
        });
    }
}


/// Rough per-message token estimate: ~4 chars/token, plus a little per-message
/// framing overhead. Shared by the send-cap and the §3.2 assembled-context line
/// so the two can't disagree.
fn estimate_message(m: &Message) -> usize {
    let mut chars = m.content.chars().count();
    if let Some(r) = &m.reasoning {
        chars += r.chars().count();
    }
    for tc in &m.tool_calls {
        chars += tc.arguments.chars().count();
    }
    chars / 4 + 4
}



/// Drop the oldest whole turns from `messages` until the estimated token count fits
/// `budget`, keeping the leading system prompt and the most recent turns. Trims in
/// WHOLE turns (a `user` message and everything up to the next `user` message), so
/// tool_use/tool_result pairs are never split and the kept history still begins at a
/// user turn (required by the Anthropic backend). Always keeps at least the last
/// turn. The token count is a rough chars/4 estimate — good enough for a
/// pricing-tier guard, but leave headroom below the exact threshold you're targeting
/// (tokenization varies, and tool-schema definitions aren't counted here). The
/// volatile tail is appended AFTER this cap (run_turn), so it can never be dropped.
fn cap_context(messages: &[Message], budget: usize) -> Vec<Message> {
    let total: usize = messages.iter().map(estimate_message).sum();
    if total <= budget {
        return messages.to_vec();
    }
    // Leading system messages are always kept.
    let sys_end = messages
        .iter()
        .position(|m| !matches!(m.role, Role::System))
        .unwrap_or(messages.len());
    let sys_tokens: usize = messages[..sys_end].iter().map(estimate_message).sum();
    let body = &messages[sys_end..];
    // Turn boundaries: each user message starts a turn.
    let starts: Vec<usize> = body
        .iter()
        .enumerate()
        .filter(|(_, m)| matches!(m.role, Role::User))
        .map(|(i, _)| i)
        .collect();
    let Some(&last_start) = starts.last() else {
        // No user turns to trim on (unusual) — don't risk orphaning a tool pair.
        return messages.to_vec();
    };
    // Always keep the last turn; add older whole turns, newest-first, while they fit.
    let mut chosen = last_start;
    let mut used = sys_tokens
        + body[last_start..]
            .iter()
            .map(estimate_message)
            .sum::<usize>();
    for &s in starts.iter().rev().skip(1) {
        let add: usize = body[s..chosen].iter().map(estimate_message).sum();
        if used + add > budget {
            break;
        }
        used += add;
        chosen = s;
    }
    let mut result: Vec<Message> = messages[..sys_end].to_vec();
    result.extend_from_slice(&body[chosen..]);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_msg(role: Role, content: &str) -> Message {
        Message {
            role,
            content: content.to_string(),
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

    fn make_tool_call_msg(content: &str, tool_args: &str) -> Message {
        Message {
            role: Role::Assistant,
            content: content.to_string(),
            images: Vec::new(),
            tool_calls: vec![ToolCall {
                id: "call_1".to_string(),
                name: "run_command".to_string(),
                arguments: tool_args.to_string(),
            }],
            tool_call_id: None,
            created_at: None,
            reasoning: None,
            reasoning_signature: None,
            sources: Vec::new(),
            segments: None,
        }
    }

    fn make_tool_result_msg(call_id: &str, content: &str) -> Message {
        Message {
            role: Role::Tool,
            content: content.to_string(),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id.to_string()),
            created_at: None,
            reasoning: None,
            reasoning_signature: None,
            sources: Vec::new(),
            segments: None,
        }
    }

    #[test]
    fn test_estimate_message_calculation() {
        // Base overhead is 4 tokens. Each 4 chars adds 1 token.
        let m_empty = make_msg(Role::User, "");
        assert_eq!(estimate_message(&m_empty), 4);

        let m_8chars = make_msg(Role::User, "12345678");
        assert_eq!(estimate_message(&m_8chars), 8 / 4 + 4); // 6

        // Arguments in tool calls are counted toward the estimate
        let m_tc = make_tool_call_msg("1234", "12345678");
        assert_eq!(estimate_message(&m_tc), (4 + 8) / 4 + 4); // 7
    }

    #[test]
    fn test_cap_context_returns_intact_when_under_budget() {
        let msgs = vec![
            make_msg(Role::System, "You are Outpost."),
            make_msg(Role::User, "Hello"),
            make_msg(Role::Assistant, "Hi there!"),
            make_msg(Role::User, "How are you?"),
            make_msg(Role::Assistant, "I am doing well."),
        ];
        let total: usize = msgs.iter().map(estimate_message).sum();

        let capped = cap_context(&msgs, total + 100);
        assert_eq!(capped.len(), msgs.len());
        assert_eq!(capped[0].content, "You are Outpost.");
        assert_eq!(capped[4].content, "I am doing well.");
    }

    #[test]
    fn test_cap_context_drops_oldest_whole_turn_when_over_budget() {
        let msgs = vec![
            make_msg(Role::System, "System prompt"),
            make_msg(Role::User, "Turn 1 question"),
            make_msg(Role::Assistant, "Turn 1 answer"),
            make_msg(Role::User, "Turn 2 question"),
            make_msg(Role::Assistant, "Turn 2 answer"),
            make_msg(Role::User, "Turn 3 question"),
            make_msg(Role::Assistant, "Turn 3 answer"),
        ];

        let sys_tokens = estimate_message(&msgs[0]);
        let turn1_tokens = estimate_message(&msgs[1]) + estimate_message(&msgs[2]);
        let turn2_tokens = estimate_message(&msgs[3]) + estimate_message(&msgs[4]);
        let turn3_tokens = estimate_message(&msgs[5]) + estimate_message(&msgs[6]);

        // Budget fits System + Turn 2 + Turn 3, but not Turn 1
        let budget = sys_tokens + turn2_tokens + turn3_tokens;
        assert!(budget < sys_tokens + turn1_tokens + turn2_tokens + turn3_tokens);

        let capped = cap_context(&msgs, budget);
        // Expect System prompt + Turn 2 + Turn 3 (5 messages total)
        assert_eq!(capped.len(), 5);
        assert_eq!(capped[0].content, "System prompt");
        assert_eq!(capped[1].content, "Turn 2 question");
        assert_eq!(capped[2].content, "Turn 2 answer");
        assert_eq!(capped[3].content, "Turn 3 question");
        assert_eq!(capped[4].content, "Turn 3 answer");
    }

    #[test]
    fn test_cap_context_drops_multiple_turns_when_budget_tight() {
        let msgs = vec![
            make_msg(Role::System, "System prompt"),
            make_msg(Role::User, "Turn 1 question"),
            make_msg(Role::Assistant, "Turn 1 answer"),
            make_msg(Role::User, "Turn 2 question"),
            make_msg(Role::Assistant, "Turn 2 answer"),
            make_msg(Role::User, "Turn 3 question"),
            make_msg(Role::Assistant, "Turn 3 answer"),
        ];

        let sys_tokens = estimate_message(&msgs[0]);
        let turn3_tokens = estimate_message(&msgs[5]) + estimate_message(&msgs[6]);

        // Budget fits System + Turn 3 only
        let budget = sys_tokens + turn3_tokens;
        let capped = cap_context(&msgs, budget);

        assert_eq!(capped.len(), 3);
        assert_eq!(capped[0].content, "System prompt");
        assert_eq!(capped[1].content, "Turn 3 question");
        assert_eq!(capped[2].content, "Turn 3 answer");
    }

    #[test]
    fn test_cap_context_always_keeps_last_turn_even_if_exceeding_budget() {
        let msgs = vec![
            make_msg(Role::System, "System prompt"),
            make_msg(Role::User, "Turn 1 question"),
            make_msg(Role::Assistant, "Turn 1 answer"),
            make_msg(Role::User, "Turn 2 with very very long content that exceeds budget by itself"),
            make_msg(Role::Assistant, "Turn 2 answer with even more text to exceed budget"),
        ];

        // Budget is unreasonably tiny (e.g. 5 tokens, while even sys + turn 2 is >30)
        let capped = cap_context(&msgs, 5);

        // System prompt and the entire last turn are still preserved
        assert_eq!(capped.len(), 3);
        assert_eq!(capped[0].content, "System prompt");
        assert_eq!(capped[1].content, "Turn 2 with very very long content that exceeds budget by itself");
        assert_eq!(capped[2].content, "Turn 2 answer with even more text to exceed budget");
    }

    #[test]
    fn test_cap_context_preserves_multi_message_turns_without_splitting() {
        // A user turn with tool calls and tool responses:
        // User -> Assistant(tool_call) -> Tool(tool_result) -> Assistant(final reply)
        let msgs = vec![
            make_msg(Role::System, "System prompt"),
            make_msg(Role::User, "Run test"),
            make_tool_call_msg("Running test now", "cargo test"),
            make_tool_result_msg("call_1", "test result: ok. 10 passed"),
            make_msg(Role::Assistant, "All tests passed."),
            make_msg(Role::User, "Next turn: git status"),
            make_msg(Role::Assistant, "Working directory is clean."),
        ];

        let sys_tokens = estimate_message(&msgs[0]);
        let turn2_tokens = estimate_message(&msgs[5]) + estimate_message(&msgs[6]);

        // Budget only fits Turn 2
        let capped = cap_context(&msgs, sys_tokens + turn2_tokens);

        // Turn 1's entire tool loop is dropped together without leaving orphan tool results
        assert_eq!(capped.len(), 3);
        assert_eq!(capped[0].content, "System prompt");
        assert_eq!(capped[1].content, "Next turn: git status");
        assert_eq!(capped[2].content, "Working directory is clean.");
    }

    #[test]
    fn test_cap_context_preserves_multiple_leading_system_prompts() {
        let msgs = vec![
            make_msg(Role::System, "System 1: Persona"),
            make_msg(Role::System, "System 2: Workspace context"),
            make_msg(Role::User, "Turn 1"),
            make_msg(Role::Assistant, "Reply 1"),
            make_msg(Role::User, "Turn 2"),
            make_msg(Role::Assistant, "Reply 2"),
        ];

        let sys1_tokens = estimate_message(&msgs[0]);
        let sys2_tokens = estimate_message(&msgs[1]);
        let turn2_tokens = estimate_message(&msgs[4]) + estimate_message(&msgs[5]);

        // Budget only fits systems + turn 2
        let capped = cap_context(&msgs, sys1_tokens + sys2_tokens + turn2_tokens);

        assert_eq!(capped.len(), 4);
        assert_eq!(capped[0].content, "System 1: Persona");
        assert_eq!(capped[1].content, "System 2: Workspace context");
        assert_eq!(capped[2].content, "Turn 2");
        assert_eq!(capped[3].content, "Reply 2");
    }

    #[test]
    fn test_cap_context_no_user_turns_returns_intact() {
        let msgs = vec![
            make_msg(Role::System, "System 1"),
            make_msg(Role::System, "System 2"),
        ];
        let capped = cap_context(&msgs, 2);
        assert_eq!(capped.len(), 2);
    }
}


