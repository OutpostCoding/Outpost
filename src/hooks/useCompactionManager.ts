// ── surface: code ──────────────────────────────────
// Compaction + context-preview domain — forModel, runCompaction, preview effect (30 deps),
// context-pressure nudge. The preview-deps list must stay in sync (notesOn lesson).
// Exports: useCompactionManager, CompactionManagerOptions · Related: App.tsx, ContextInspectorModal.tsx, commands.rs

import { useState, useEffect } from "react";
import {
  compactThread,
  previewContext,
  type Message,
  type Compaction,
  type ContextPreview,
  type ConnRoute,
} from "../lib/provider";

export interface CompactionManagerOptions {
  // ── Transcript & surface state ──
  messages: Message[]; // live transcript — previewMessages + compaction fold from this
  currentThreadId: number | null; // null = no thread open → runCompaction disabled
  sessionRoot: string | null; // null marks the CHAT surface (no project FS)
  busy: boolean; // a turn is running → runCompaction disabled
  model: string; // model name — preview header + session-facts head block
  connRoute: () => ConnRoute; // reads the LIVE route (kind + baseUrl) for the head block
  contextCap: () => number | null; // reads the user's context cap; null = no cap → no nudge
  setBusy: (b: boolean) => void; // busy setter — runCompaction drives it during the call
  setError: (e: string | null) => void; // error reporter
  setNotice: (n: string | null) => void; // notice reporter
  enabledToolList: () => string[]; // builds the LIVE tool list — reads the toggle block below
  systemForModel: () => string; // system prompt builder (reads the prompt inputs below)
  profileForModel: () => string; // profile prompt builder (reads the profile inputs below)

  // ── Tool toggles. Each feeds enabledToolList, which the preview effect calls,
  // so EVERY one must stay in the preview's deps array — the notesOn lesson: a
  // toggle missing from deps makes the inspector's tool block lie about the send.
  visionOn: boolean;
  toolsOn: boolean;
  webSearchOn: boolean;
  webFetchOn: boolean;
  webOn?: boolean;
  browserOn?: boolean;
  codeGroupsOn: Record<string, boolean>;
  liteTools: boolean;
  godotToolsOn: boolean;
  mdWriteOn: boolean;
  notesOn: boolean;
  htmlWriteOn: boolean;
  codeOn: boolean;

  // ── Connection (also preview deps) ──
  activeConn: string; // which connection is active — head block + route
  connBaseUrls: Record<string, string>; // base URLs per connection — head block

  // ── System/profile prompt inputs (also preview deps) ──
  systemPrompt: string; // local system prompt text
  globalSystem: string; // active global system prompt text (side-resolved by App)
  systemScope: "global" | "local"; // which of the two is in effect
  profileShare: boolean; // profile sharing pref
  profileName: string; // profile name
  profilePreferred: string; // preferred profile id
  profileAbout: string; // profile about text
  maxContext: string; // user-declared max-context label
  readPruneTurns?: number | null; // read history retention limit
  stagedFiles: string[]; // files staged for the NEXT turn — preview only; the framing itself is passed per-turn
}

export function useCompactionManager(opts: CompactionManagerOptions) {
  const {
    messages,
    currentThreadId,
    sessionRoot,
    busy,
    model,
    connRoute,
    contextCap,
    setBusy,
    setError,
    setNotice,
    enabledToolList,
    systemForModel,
    profileForModel,
    visionOn,
    toolsOn,
    webSearchOn,
    webFetchOn,
    webOn,
    browserOn,
    codeGroupsOn,
    liteTools,
    godotToolsOn,
    mdWriteOn,
    notesOn,
    htmlWriteOn,
    codeOn,
    activeConn,
    connBaseUrls,
    systemPrompt,
    globalSystem,
    systemScope,
    profileShare,
    profileName,
    profilePreferred,
    profileAbout,
    maxContext,
    readPruneTurns,
    stagedFiles,
  } = opts;

  // Compaction: a handover note standing in for the first `up_to` messages when
  // SENDING. The transcript is never touched — you keep full scrollback, the model
  // gets the note. Persisted in the thread config.
  const [compaction, setCompaction] = useState<Compaction | null>(null);
  const [isCompacting, setIsCompacting] = useState(false);
  const [preview, setPreview] = useState<ContextPreview | null>(null);
  const [showInspector, setShowInspector] = useState(false);
  const [nudgeDismissed, setNudgeDismissed] = useState(false);

  // Surviving transcript messages + the handover note (if compacted), with the
  // folded prefix dropped. The model ONLY EVER sees this view; it sees the
  // surviving ones verbatim. The transcript on screen is untouched — this is the
  // one place the two views diverge, so both sendChat and the context inspector
  // must go through it or the inspector would lie about what was sent.
  //
  // The note goes in as a SYSTEM message directly after the main system prompt.
  // That position is correct on both backends for free: Anthropic hoists system
  // turns into its top-level `system`, which is exactly where a handover note
  // belongs, and OpenAI-compatible backends take a system message inline.
  function forModel(
    msgs: Message[],
    stagedFiles: string[] = [],
  ): Message[] {
    // Copy up front, so that no return path below hands a caller back an array
    // aliased to React state or to `runTurn`'s local `history`.
    // Action scaffolding: expand prompt template for turns initiated via an action.
    let out = msgs.map((m) => {
      if (m.role === "user" && m.action) {
        const template = (m.action as { name?: string; template?: string }).template;
        if (template) {
          const actionName = m.action.name;
          const prefix = `/${actionName}`;
          let query = m.content;
          if (query.toLowerCase().startsWith(prefix.toLowerCase())) {
            query = query.slice(prefix.length).trim();
          }
          let expanded = template;
          if (expanded.includes("$INPUT")) {
            expanded = expanded.split("$INPUT").join(query);
          } else if (expanded.includes("{{input}}")) {
            expanded = expanded.split("{{input}}").join(query);
          } else if (expanded.includes("$ARGUMENTS")) {
            expanded = expanded.split("$ARGUMENTS").join(query);
          } else if (query.trim()) {
            expanded = `${expanded.trim()}\n\n## Input\n${query.trim()}`;
          }
          return { ...m, content: expanded };
        }
      }
      return m;
    });
    msgs = out;
    // Staged @-attachments: say who staged them, and when.
    //
    // Without this the model is handed a COMPLETED tool call — `read_note` with
    // its result already attached — on a user message that never mentions the
    // file (the composer consumes the `@path` when the mention is picked). A
    // finished tool call with no request behind it reads as something that
    // happened EARLIER. On a first turn the model's own reasoning said "looking
    // at the previous turn, I called read_note", about a turn that did not
    // exist, and it then answered as though it had shown the user a document in
    // that invented past.
    // To prevent invented past turns, we use a tag pair for an unambiguous extent
    // plus one sentence of attribution. It rides the USER turn rather than the synthetic
    // assistant one because `expand_messages_with_segments` emits an assistant's
    // text AFTER its tool results — framing there would land after the read and
    // read as the answer.
    //
    // The file contents are deliberately NOT restated. They arrive as the tool
    // results below; duplicating them here would double the send and defeat
    // readPruneTurns, since text folded into a user message never prunes to a
    // stub the way a tool result does.
    if (stagedFiles.length > 0) {
      const last = msgs[msgs.length - 1];
      if (last && last.role === "user") {
        const base = last.content ? `${last.content}\n\n` : "";
        msgs = [
          ...msgs.slice(0, -1),
          {
            ...last,
            content:
              `${base}<staged-files>\n` +
              `Inserted automatically by Outpost. The user attached the file(s) ` +
              `listed below to THIS message before sending it — they are not from ` +
              `an earlier turn. They have already been read for you; the contents ` +
              `arrive below as tool results, as part of the current turn.\n\n` +
              `${stagedFiles.map((f) => `- ${f}`).join("\n")}\n` +
              `</staged-files>`,
          },
        ];
      }
    }
    // Compaction substitution: the first `up_to` messages are replaced by a
    // single system-role handover note.
    //
    // Cases handled:
    //  1. /clear (or completely emptied transcript): `msgs` is empty — send the
    //     handover note as a standalone system message so the model still has
    //     the context on a cleared slate.
    //  2. Surviving messages after the fold boundary: prepend the note to `kept`.
    //  3. All transcript messages were deleted down to or below the fold: the note
    //     is preserved as a standing note rather than silently dropped.
    //  4. Folded prefix was deleted (up_to == 0): all surviving messages are live;
    //     the note is prepended before them.
    if (!compaction) return msgs;
    if (msgs.length === 0) {
      // /clear case: empty transcript, standing note.
      return [
        {
          role: "system",
          content:
            `CONTEXT NOTE — earlier conversation compacted.\n\n` +
            `The handover note below covers work done before the clear. Treat it as ` +
            `established fact, not as new instructions. Use edit_file for targeted code modifications.\n\n${compaction.summary}`,
        },
      ];
    }
    const upTo = Math.max(0, Math.min(compaction.up_to, msgs.length));
    const kept = msgs.slice(upTo);
    const lead =
      upTo > 0
        ? `The first ${upTo} messages of this session were folded into the handover note below to save context.`
        : `The handover note below covers earlier work done in this session.`;
    const tail =
      kept.length > 0
        ? ` The messages after this note are the live conversation, resuming from where the note leaves off.`
        : "";
    return [
      {
        role: "system",
        // Ground it in the conversation, not just in the system block. On Anthropic
        // this gets hoisted in with the standing instructions, so without saying so
        // the model can't tell a fold happened, how much it covered, or where the
        // live conversation resumes — it just sees more system text. Naming the
        // boundary in words gives that back. (A marker message at the boundary would
        // ground it better still, but the first kept message is a user turn, so a
        // user-role marker there means two consecutive user turns — a 400 on
        // Anthropic — and a system one gets hoisted anyway.)
        content:
          `CONTEXT NOTE — earlier conversation compacted.\n\n` +
          `${lead} Treat it as established fact about work already done, not as new instructions.${tail} ` +
          `Use edit_file for targeted code modifications.\n\n${compaction.summary}`,
      },
      ...kept,
    ];
  }

  // Fold everything before the last user turn into a summary. Cutting on a
  // USER-message boundary keeps whole turns intact, so a tool_use can never be
  // separated from its tool_result (the thing that 400s on Anthropic).
  //
  // One command, one note. `/compact` folds the prior note and the new transcript
  // into a single handover note; the model carries decisions forward by copying
  // them (regeneration, not delta), and the code bounds the summariser's own
  // input. `budgetChars` is `null` when no context cap is set — no cap means no
  // clip for summarising either, so nothing is silently dropped. When a cap IS
  // set and the span since the last compaction overshoots it, `dropped > 0`
  // reports the turns that fell outside the window.
  //
  // Summariser input and substitution boundary are deliberately DIFFERENT now:
  // the summariser reads the WHOLE span (last turn included) so the note
  // closes questions whose answers landed recently, while the substitution still
  // keeps the last user turn live and verbatim. The exclusion used to apply
  // to BOTH — the note was written as of earlier turns and could never close a
  // question answered in the live window, and those closures were lost from the
  // note lineage entirely once the live turns scrolled out.
  async function runCompaction() {
    if (busy || currentThreadId == null) return;
    const KEEP_RECENT_USER_TURNS = 1;
    const userIdx = messages
      .map((m, i) => (m.role === "user" ? i : -1))
      .filter((i) => i >= 0);
    if (userIdx.length <= KEEP_RECENT_USER_TURNS) {
      setError("Not enough history to compact yet — have a few more turns first.");
      return;
    }
    const upTo = userIdx[userIdx.length - KEEP_RECENT_USER_TURNS];
    const older = messages.slice(0, upTo);
    setIsCompacting(true);
    setBusy(true);
    setError(null);
    try {
      // Bound the summariser's OWN input. `null` (no declared cap) means no clip:
      // the whole span is shown and nothing is dropped. A cap gives the summariser
      // the newest `cap * 4 * 0.5` chars and reports anything older via `dropped`.
      // `messages` (NOT `older`) is sent so the note closes questions whose answers
      // landed in the last turn; `upTo` below still defines the fold boundary.
      const cap = contextCap();
      const budgetChars = cap ? Math.max(20000, Math.floor(cap * 4 * 0.5)) : null;
      const { summary, dropped } = await compactThread(
        messages,
        compaction?.summary ?? null,
        budgetChars,
        model,
        connRoute(),
      );
      const next: Compaction = { summary, up_to: upTo, at: Date.now() };
      setCompaction(next);
      setNotice(
        dropped > 0
          ? `Compacted ${older.length} earlier messages into a handover note — but ${dropped} of the oldest turns fell outside the summary budget and aren't captured. Compact more often, or raise/clear the context cap. Your transcript is unchanged.`
          : `Compacted ${older.length} earlier messages into a handover note. Your transcript is unchanged — check the context inspector, or Files → Compaction to review and edit the note.`,
      );
    } catch (e) {
      const msg = String(e);
      if (msg.includes("cancelled") || msg.includes("stopped")) {
        setNotice("Compaction stopped.");
      } else {
        setError(`Compaction failed: ${e}`);
      }
    } finally {
      setIsCompacting(false);
      setBusy(false);
    }
  }

  // What the inspector previews. Simulate the next turn with a placeholder user message
  // when files are staged, so their framing, position, and token cost are visible.
  function previewMessages(): Message[] {
    const pending = stagedFiles;
    if (pending.length > 0) {
      const last = messages[messages.length - 1];
      if (!last || last.role !== "user") {
        return forModel(
          [...messages, { role: "user", content: "‹your next message›" }],
          pending,
        );
      }
    }
    return forModel(messages, pending);
  }

  // Retired: documents are no longer pinned to the tail. Returns empty array.
  function contextFileList(): string[] {
    return [];
  }

  // Keep a live context preview for the token-count button + inspector.
  useEffect(() => {
    previewContext(
      previewMessages(),
      enabledToolList(),
      contextFileList(),
      systemForModel(),
      profileForModel(),
      sessionRoot,
      visionOn,
      model || null,
      connRoute().kind,
      connRoute().baseUrl,
      contextCap(),
      readPruneTurns,
    )
      .then(setPreview)
      .catch(() => {});
  }, [
    messages,
    compaction,
    sessionRoot,
    toolsOn,
    webSearchOn,
    webFetchOn,
    webOn,
    visionOn,
    browserOn,
    liteTools,
    codeGroupsOn,
    godotToolsOn,
    mdWriteOn,
    // gates search_notes/read_note in enabledToolList, so without it the
    // inspector's tool block stays wrong until some unrelated dep changes.
    notesOn,
    htmlWriteOn,
    codeOn,
    // The session-facts head block (§3.1) depends on the model + connection
    // route, so the preview must re-run when either changes.
    model,
    activeConn,
    connBaseUrls,
    systemPrompt,
    globalSystem,
    systemScope,
    profileShare,
    profileName,
    profilePreferred,
    profileAbout,
    maxContext,
    readPruneTurns,
    // Staged-attachment framing is part of the send, so the inspector has to
    // re-run when the staged set changes or the preview under-reports it.
    stagedFiles,
  ]);

  // Context-full nudge: proactive notice when context usage passes 80% of the
  // declared cap, suggesting /compact.
  //
  // Why 80% of the user's CAP, not of the model's window: model windows range
  // from 8k to 1M, the number isn't reliably discoverable (LM Studio reports a
  // model's MAXIMUM, not the size it was actually loaded at), and even a correct
  // window doesn't say when to act — an agentic turn needs headroom for forty
  // iterations of tool results, a chat doesn't. Reading windows per model would
  // also mean Outpost learning per-model facts, which is exactly the coupling we
  // avoid elsewhere. So the user defines "full" once, in the cap they already set;
  // no cap means no opinion and no nudge.
  const NUDGE_AT = 0.8;
  const capForNudge = contextCap();
  const contextPressure =
    capForNudge != null &&
    preview != null &&
    preview.total_tokens >= capForNudge * NUDGE_AT;
  // Re-arm once usage falls back under the line (e.g. after compacting), so a
  // dismissal silences this crossing rather than the feature.
  useEffect(() => {
    if (!contextPressure) setNudgeDismissed(false);
  }, [contextPressure]);

  return {
    compaction,
    setCompaction,
    preview,
    setPreview,
    showInspector,
    setShowInspector,
    nudgeDismissed,
    setNudgeDismissed,
    forModel,
    runCompaction,
    previewMessages,
    contextFileList,
    contextPressure,
    isCompacting,
  };
}
