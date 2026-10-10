// ── surface: chat ──────────────────────────────────
// Collapsible work block (interleaved reasoning text + tool chips with line down the side).
// Exports: ThinkingGroup · Related: TurnTimeline.tsx, ToolChip.tsx

import { useState } from "react";
import type { Segment } from "../../lib/provider";
import { stripCodeFences } from "../../utils";
import { Markdown } from "../markdown/Markdown";
import { ToolChip } from "./ToolChip";

function ThinkingBlock({ text }: { text: string }) {
  const [open, setOpen] = useState(false);
  const clean = stripCodeFences(text);
  if (!clean.trim()) return null;

  const firstLine =
    clean
      .split("\n")
      .map((s) => s.trim())
      .find(Boolean) ?? "";
  const cleanedSummary = firstLine.replace(/^[#*-\s]+/, "").trim();
  const summary =
    cleanedSummary.length > 90
      ? cleanedSummary.slice(0, 90).trimEnd() + "…"
      : cleanedSummary;

  const label = open ? "thinking" : summary || "thinking";

  return (
    <div className="thinking-sub">
      <button
        type="button"
        className="thinking-head thinking-sub-head"
        onClick={() => setOpen((o) => !o)}
        title={open ? "Collapse thinking" : clean.slice(0, 300)}
      >
        <span className={`thinking-chevron${open ? " open" : ""}`}>▸</span>
        <span className="thinking-label">{label}</span>
      </button>
      {open && (
        <div className="thinking-sub-body">
          <Markdown text={clean} />
        </div>
      )}
    </div>
  );
}

export function ThinkingGroup({ segs, live }: { segs: Segment[]; live?: boolean }) {
  const [open, setOpen] = useState(false);
  const reasoningSegs = segs.filter((s) => s.kind === "reasoning");
  const toolSegs = segs.filter((s) => s.kind === "tool");
  const hasReasoning = reasoningSegs.some((s) => s.text.trim().length > 0);
  const toolCount = toolSegs.length;

  if (toolCount === 0) {
    const firstReasoning = reasoningSegs[0];
    const firstLine =
      (firstReasoning && firstReasoning.kind === "reasoning" ? firstReasoning.text : "")
        .split("\n")
        .map((s) => s.trim())
        .find(Boolean) ?? "";
    const summary =
      firstLine.length > 140 ? firstLine.slice(0, 140).trimEnd() + "…" : firstLine;
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
          <span className={`thinking-chevron${open ? " open" : ""}`}>▸</span>
          <span className="thinking-label">{label}</span>
        </button>
        {open && (
          <div className="thinking-body">
            {segs.map((s, i) => {
              if (s.kind === "reasoning") {
                const clean = stripCodeFences(s.text);
                if (!clean.trim()) return null;
                return <Markdown key={i} text={clean} />;
              }
              return null;
            })}
          </div>
        )}
      </div>
    );
  }

  let label = "";
  if (live) {
    if (hasReasoning && toolCount > 0) label = "Thinking & working…";
    else if (hasReasoning) label = "Thinking…";
    else label = `Running ${toolCount === 1 ? "tool" : "tools"}…`;
  } else {
    // Completed turn
    label = `Work · ${toolCount} ${toolCount === 1 ? "action" : "actions"}${hasReasoning ? ", thinking" : ""}`;
  }

  return (
    <div className={`thinking${live ? " live" : ""}`}>
      <button
        type="button"
        className="thinking-head"
        onClick={() => setOpen((o) => !o)}
      >
        <span className={`thinking-chevron${open ? " open" : ""}`}>▸</span>
        <span className="thinking-label">{label}</span>
      </button>
      {open && (
        <div className="thinking-body">
          {segs.map((s, i) => {
            if (s.kind === "reasoning") {
              return <ThinkingBlock key={i} text={s.text} />;
            }
            if (s.kind === "tool") {
              return <ToolChip key={i} seg={s} />;
            }
            return null;
          })}
        </div>
      )}
    </div>
  );
}
