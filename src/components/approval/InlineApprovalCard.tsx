// ── surface: shared ──────────────────────────────────
// Inline tool approval card — renders directly in the transcript timeline
// when a tool call requires human confirmation. Context remains 100% visible
// and scrollable; buttons and inputs fold away immediately upon decision.
// Exports: InlineApprovalCard · Related: TurnTimeline.tsx, ChatSurface.tsx, DiffView.tsx

import { useState, useEffect, useRef } from "react";
import { toolIcon, PREVIEW_AS_TEXT, APPROVE_LABEL, type PendingEdit } from "../../utils";
import { DiffView } from "../ui/DiffView";

export function InlineApprovalCard({
  pendingEdit,
  onAnswer,
}: {
  pendingEdit: PendingEdit;
  onAnswer: (decision: "approve" | "reject" | "remember", feedback?: string) => void;
}) {
  const [feedback, setFeedback] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);
  const cardRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    setFeedback("");
  }, [pendingEdit.id]);

  // Take focus when the card appears, and hand it back when it clears. Without this
  // the card's global Enter/Esc binding only fired when the card *happened* to hold
  // focus — but nothing focused it, and a keyboard send leaves focus in the composer
  // (so the next Enter did nothing) while a mouse send can leave it on the Send/Stop
  // button (so Enter hit Stop and killed the turn). Focusing the container — not a
  // button, which would double-fire — makes Enter=approve / Esc=reject authoritative
  // the moment the approval appears, however the turn was sent. The skip rule still
  // lets you click into a field to type without the shortcuts stealing keystrokes.
  useEffect(() => {
    const previous = document.activeElement as HTMLElement | null;
    cardRef.current?.focus({ preventScroll: true });
    return () => {
      if (previous && document.contains(previous)) previous.focus();
    };
  }, []);

  const trimmed = feedback.trim();

  // The card advertises "press Enter to approve" / "Esc to reject" on its
  // buttons, but it is not itself focusable and nothing autofocuses it — so
  // without a window listener those keys only worked while the feedback input
  // held focus. Bind them globally while the card is mounted. Skip elements
  // that already handle Enter/Esc natively (the feedback input, buttons, the
  // composer) so we never hijack typing or double-fire a focused button.
  useEffect(() => {
    function onKeyDown(e: KeyboardEvent) {
      const el = document.activeElement as HTMLElement | null;
      if (el) {
        const tag = el.tagName;
        if (
          tag === "INPUT" ||
          tag === "TEXTAREA" ||
          tag === "BUTTON" ||
          tag === "SELECT" ||
          tag === "A" ||
          el.isContentEditable
        ) {
          return;
        }
      }
      if (e.key === "Escape") {
        e.preventDefault();
        onAnswer("reject", trimmed || undefined);
      } else if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        onAnswer("approve");
      }
    }
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [onAnswer, trimmed]);

  // "Always allow" only appears when the backend found a pattern safe to persist.
  // A command that fails a guard — shell chaining, a pipe or redirect (`2>&1`!),
  // a path outside the project — can NEVER auto-approve, so whitelisting a token
  // would silently do nothing. The button is OMITTED rather than shown dead with
  // the command's first word, which read as "this is what it would add" when it
  // wasn't. (`remember` is the leading token of every segment, so a compound like
  // `git add -A && git commit` yields just `git`.)
  const remember = pendingEdit.command?.remember ?? [];
  const canRemember = remember.length > 0;

  return (
    <div
      ref={cardRef}
      tabIndex={-1}
      className="inline-approval"
      role="region"
      aria-label="Tool approval required"
    >
      <div className="inline-approval-header">
        <div className="inline-approval-meta">
          <span className="inline-approval-badge">Action Required</span>
          <span className="inline-approval-tool">
            {toolIcon(pendingEdit.tool)} <strong>{pendingEdit.tool}</strong>
          </span>
          <span className="inline-approval-arrow">→</span>
          <code className="inline-approval-path" title={pendingEdit.path}>
            {pendingEdit.path}
          </code>
        </div>
        <div className="inline-approval-hints">
          <button
            type="button"
            className="kbd-hint kbd-hint-approve"
            onClick={() => onAnswer("approve")}
            title="Click or press Enter to approve"
          >
            ↵ Approve
          </button>
          <button
            type="button"
            className="kbd-hint kbd-hint-reject"
            onClick={() => onAnswer("reject", trimmed || undefined)}
            title="Click or press Esc to reject"
          >
            Esc Reject
          </button>
        </div>
      </div>

      {pendingEdit.warning && (
        <div className="inline-approval-warn">
          <span className="inline-approval-warn-icon">⚠</span>
          <span>{pendingEdit.warning}</span>
        </div>
      )}

      <div className="inline-approval-preview">
        {PREVIEW_AS_TEXT.has(pendingEdit.tool) ? (
          <pre className="command-preview inline-preview">{pendingEdit.diff}</pre>
        ) : (
          <div className="inline-diff-wrapper">
            <DiffView diff={pendingEdit.diff} />
          </div>
        )}
      </div>

      <div className="inline-approval-feedback">
        <input
          ref={inputRef}
          type="text"
          className="approval-feedback-input"
          placeholder="Feedback for model on reject (optional, e.g. “Mock this in test”)..."
          value={feedback}
          onChange={(e) => setFeedback(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              if (trimmed) {
                onAnswer("reject", trimmed);
              } else {
                onAnswer("approve");
              }
            } else if (e.key === "Escape") {
              e.preventDefault();
              onAnswer("reject");
            }
          }}
        />
      </div>

      <div className="inline-approval-actions">
        <button
          type="button"
          className="inline-approval-btn approve"
          onClick={() => onAnswer("approve")}
          title="Approve this tool call (Enter)"
        >
          {APPROVE_LABEL[pendingEdit.tool] ?? "Approve & write"}
        </button>

        {canRemember && (
          <button
            type="button"
            className="inline-approval-btn remember"
            onClick={() => onAnswer("remember")}
            title={`Approve, and always allow ${remember.join(", ")} so it stops prompting`}
          >
            Always allow <code>{remember.join(", ")}</code>
          </button>
        )}

        <button
          type="button"
          className={`inline-approval-btn reject ${trimmed ? "has-feedback" : ""}`}
          onClick={() => onAnswer("reject", trimmed || undefined)}
          title={
            trimmed
              ? "Reject and send feedback note to the model (Enter)"
              : "Reject this tool call (Esc)"
          }
        >
          {trimmed ? "Reject with note" : "Reject"}
        </button>
      </div>
    </div>
  );
}
