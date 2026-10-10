// ── surface: shared ──────────────────────────────────
// System-prompt / instructions viewer-editor.
// Exports: InstructionsModal · Related: App.tsx (activeSystem)
//
// Three scopes (split 2026-08-24): Global · Chat, Global · Code, and This chat.
// The thread-level `systemScope` stays "global" | "local" — "global" resolves
// to whichever side the user is currently on (App's `activeSystem`); this modal
// just edits the two global texts separately so a fire-and-forget prompt can
// exist for each side.

import { useBackdropClose } from "../hooks/useBackdropClose";

export function InstructionsModal({
  showSystem,
  onClose,
  draftScope,
  setDraftScope,
  globalSystemChat,
  globalSystemCode,
  systemPrompt,
  systemDraft,
  setSystemDraft,
  setSystemScope,
  setGlobalSystemChat,
  setGlobalSystemCode,
  setSystemPrompt,
}: {
  showSystem: boolean;
  onClose: () => void;
  draftScope: "global_chat" | "global_code" | "local";
  setDraftScope: (s: "global_chat" | "global_code" | "local") => void;
  globalSystemChat: string;
  globalSystemCode: string;
  systemPrompt: string;
  systemDraft: string;
  setSystemDraft: (d: string) => void;
  setSystemScope: (s: "global" | "local") => void;
  setGlobalSystemChat: (s: string) => void;
  setGlobalSystemCode: (s: string) => void;
  setSystemPrompt: (p: string) => void;
}) {
  const backdropHandlers = useBackdropClose(onClose);

  if (!showSystem) return null;

  return (
    <div className="export-overlay" {...backdropHandlers}>
      <div
        className="export instructions-modal"
        onClick={(e) => e.stopPropagation()}
        onMouseDown={(e) => e.stopPropagation()}
      >
        <div className="export-head">
          <h3>Instructions</h3>
          <div className="export-views">
            <button
              className={draftScope === "global_chat" ? "active" : ""}
              onClick={() => {
                setDraftScope("global_chat");
                setSystemDraft(globalSystemChat);
              }}
            >
              Global · Chat
            </button>
            <button
              className={draftScope === "global_code" ? "active" : ""}
              onClick={() => {
                setDraftScope("global_code");
                setSystemDraft(globalSystemCode);
              }}
            >
              Global · Code
            </button>
            <button
              className={draftScope === "local" ? "active" : ""}
              onClick={() => {
                setDraftScope("local");
                setSystemDraft(systemPrompt);
              }}
            >
              This chat
            </button>
          </div>
        </div>
        <div className="export-preview">
          <textarea
            className="sys-editor"
            value={systemDraft}
            onChange={(e) => setSystemDraft(e.target.value)}
            placeholder="e.g. You are a terse senior engineer. Prefer code over prose, and ask before making assumptions…"
            autoFocus
          />
        </div>
        <div className="export-foot">
          <span className="export-saved">
            {draftScope === "global_chat"
              ? "Applies to every chat on the Chat side"
              : draftScope === "global_code"
                ? "Applies to every session on the Code side"
                : "This chat only"}
          </span>
          <button onClick={onClose}>Cancel</button>
          <button
            className="primary"
            onClick={() => {
              if (draftScope === "global_chat") {
                setSystemScope("global");
                setGlobalSystemChat(systemDraft);
              } else if (draftScope === "global_code") {
                setSystemScope("global");
                setGlobalSystemCode(systemDraft);
              } else {
                setSystemScope("local");
                setSystemPrompt(systemDraft);
              }
              onClose();
            }}
          >
            Save
          </button>
        </div>
      </div>
    </div>
  );
}
