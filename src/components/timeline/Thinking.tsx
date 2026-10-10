// ── surface: chat ──────────────────────────────────
// Single reasoning block renderer (live or final).
// Exports: Thinking · Related: ThinkingGroup.tsx, TurnTimeline.tsx

import { useState } from "react";
import { stripCodeFences } from "../../utils";
import { Markdown } from "../markdown/Markdown";

export function Thinking({ text, live }: { text: string; live: boolean }) {
  const [open, setOpen] = useState(false);
  const clean = stripCodeFences(text);
  const firstLine =
    clean
      .split("\n")
      .map((s) => s.trim())
      .find(Boolean) ?? "";
  const summary =
    firstLine.length > 140 ? firstLine.slice(0, 140).trimEnd() + "…" : firstLine;
  const expanded = open;
  const label = live
    ? "Thinking…"
    : open
      ? "Thought process"
      : summary || "Thought process";
  return (
    <div className={`thinking${live ? " live" : ""}`}>
      <button
        type="button"
        className="thinking-head"
        onClick={() => setOpen((o) => !o)}
      >
        <span className={`thinking-chevron${expanded ? " open" : ""}`}>▸</span>
        <span className="thinking-label">{label}</span>
      </button>
      {expanded && (
        <div className="thinking-body">
          <Markdown text={clean} />
        </div>
      )}
    </div>
  );
}
