// ── surface: chat ──────────────────────────────────
// Web-source link row in a turn (search/fetch citations).
// Exports: Sources · Related: TurnTimeline.tsx, tools/search.rs

import { useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { isSafeExternalUrl } from "../../utils";

export function Sources({
  items,
  onOpenNote,
}: {
  items: { kind: string; text: string; detail?: string }[];
  onOpenNote?: (path: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const [expanded, setExpanded] = useState<Set<number>>(new Set());
  const toggleItem = (i: number) =>
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(i)) next.delete(i);
      else next.add(i);
      return next;
    });
  const icon = (k: string) =>
    k === "fetch"
      ? "≡"
      : k === "note"
        ? "≡"
        : k === "notes_search"
          ? "⌕"
          : k === "code"
            ? "▶"
            : "⌕";
  return (
    <div className="sources">
      <button
        type="button"
        className="sources-head"
        onClick={() => setOpen((o) => !o)}
      >
        <span className={`sources-chevron${open ? " open" : ""}`}>▸</span>
        {items.length} {items.length === 1 ? "source" : "sources"}
      </button>
      {open && (
        <div className="sources-body">
          {items.map((s, i) => (
            <div key={i} className="source-row">
              <div className="source-item">
                <span className="source-kind">{icon(s.kind)}</span>
                {s.kind === "fetch" ? (
                  <a
                    href={s.text}
                    onClick={(e) => {
                      e.preventDefault();
                      if (isSafeExternalUrl(s.text)) openUrl(s.text).catch(() => {});
                    }}
                  >
                    {s.text}
                  </a>
                ) : s.kind === "note" ? (
                  <button
                    type="button"
                    className="source-link"
                    title="Open in viewer"
                    onClick={() => onOpenNote?.(s.text)}
                  >
                    {s.text}
                  </button>
                ) : (
                  <span>{s.text}</span>
                )}
                {s.detail && (
                  <button
                    type="button"
                    className="source-peek"
                    onClick={() => toggleItem(i)}
                  >
                    {expanded.has(i) ? "hide" : "snippet"}
                  </button>
                )}
              </div>
              {s.detail && expanded.has(i) && (
                <pre className="source-detail">{s.detail}</pre>
              )}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
