// ── surface: code ──────────────────────────────────
// Context inspector — mirrors head/tail blocks, tool list, estimates (live preview).
// Exports: ContextInspectorModal · Related: useCompactionManager.ts (preview), commands.rs (preview_context)

import type { ContextPreview } from "../lib/provider";
import { useBackdropClose } from "../hooks/useBackdropClose";

export function ContextInspectorModal({
  showInspector,
  preview,
  onClose,
}: {
  showInspector: boolean;
  preview: ContextPreview | null;
  onClose: () => void;
}) {
  const backdropHandlers = useBackdropClose(onClose);

  if (!showInspector || !preview) return null;

  return (
    <div className="approval-overlay" {...backdropHandlers}>
      <div
        className="inspector"
        onClick={(e) => e.stopPropagation()}
        onMouseDown={(e) => e.stopPropagation()}
      >
        <div className="inspector-head">
          <span>
            Assembled context · ~{preview.total_tokens.toLocaleString()} tokens
          </span>
          <button onClick={onClose} aria-label="Close">
            ✕
          </button>
        </div>
        <div className="inspector-body">
          {preview.sections.map((s, i) =>
            s.content.trim() === "" ? (
              // Content-less sections are inspector chrome (e.g. the tail
              // divider) — render as a divider row, not an empty card.
              <div key={i} className="ctx-divider">
                {s.label}
              </div>
            ) : (
              <div
                key={i}
                className={`ctx-section ${
                  s.label.toLowerCase().includes("user") ? "ctx-section-user" : ""
                }`}
              >
                <div className="ctx-section-head">
                  <span className="ctx-label">{s.label}</span>
                  <span className="ctx-tokens">~{s.tokens.toLocaleString()} tok</span>
                </div>
                <pre className="ctx-content">{s.content}</pre>
              </div>
            )
          )}
        </div>
      </div>
    </div>
  );
}
