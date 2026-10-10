// ── surface: chat ──────────────────────────────────
// Turn renderer — assembles thinking/tool segments for one assistant turn.
// Supports inline approval gating directly within the turn timeline.
// Exports: TurnTimeline · Related: ChatSurface.tsx, ToolChip.tsx, Thinking.tsx, InlineApprovalCard.tsx

import type { Segment } from "../../lib/provider";
import { balanceFences, type PendingEdit } from "../../utils";
import { Markdown } from "../markdown/Markdown";
import { Thinking } from "./Thinking";
import { ToolChip } from "./ToolChip";
import { InlineApprovalCard } from "../approval/InlineApprovalCard";

export function TurnTimeline({
  segments,
  live,
  pendingEdit,
  onAnswerEdit,
}: {
  segments: Segment[];
  live?: boolean;
  pendingEdit?: PendingEdit | null;
  onAnswerEdit?: (decision: "approve" | "reject" | "remember", feedback?: string) => void;
}) {
  type Row =
    | { type: "text"; text: string }
    | { type: "tool"; seg: Extract<Segment, { kind: "tool" }> }
    | { type: "reasoning"; text: string };

  const rows: Row[] = [];

  for (const s of segments) {
    if (s.kind === "text") {
      if (!s.text.trim() && !live) continue;
      const prev = rows[rows.length - 1];
      if (prev && prev.type === "text") {
        prev.text += "\n\n" + s.text;
      } else {
        rows.push({ type: "text", text: s.text });
      }
    } else if (s.kind === "tool") {
      rows.push({ type: "tool", seg: s });
    } else if (s.kind === "reasoning") {
      if (!s.text.trim() && !live) continue;
      const prev = rows[rows.length - 1];
      if (prev && prev.type === "reasoning") {
        prev.text += "\n\n" + s.text;
      } else {
        rows.push({ type: "reasoning", text: s.text });
      }
    }
  }

  // Balance code fences across text bubbles so unclosed fences never invert formatting
  let activeFence: string | null = null;
  const processedRows = rows.map((row) => {
    if (row.type !== "text") return row;
    const { processedText, outgoingFence } = balanceFences(row.text, activeFence);
    activeFence = outgoingFence;
    return { ...row, text: processedText };
  });

  let approvalRendered = false;

  return (
    <>
      {processedRows.map((row, i) => {
        const isLast = i === processedRows.length - 1;
        const rowKey =
          row.type === "tool"
            ? row.seg.id || `tool-${i}`
            : `${row.type}-${i}`;

        if (row.type === "text") {
          return (
            <div
              key={rowKey}
              className={`bubble assistant${live && isLast ? " streaming" : ""}`}
            >
              <Markdown text={row.text} streaming={live && isLast} />
            </div>
          );
        }
        if (row.type === "tool") {
          const isPending =
            Boolean(pendingEdit) &&
            row.seg.output === undefined &&
            (row.seg.name === pendingEdit?.tool || isLast);

          if (isPending && pendingEdit && onAnswerEdit && !approvalRendered) {
            approvalRendered = true;
            return (
              <InlineApprovalCard
                key={`approval-${pendingEdit.id}`}
                pendingEdit={pendingEdit}
                onAnswer={onAnswerEdit}
              />
            );
          }
          return <ToolChip key={rowKey} seg={row.seg} />;
        }
        if (row.type === "reasoning") {
          return <Thinking key={rowKey} text={row.text} live={!!live && isLast} />;
        }
        return null;
      })}

      {pendingEdit && onAnswerEdit && !approvalRendered && (
        <InlineApprovalCard
          key={`approval-fallback-${pendingEdit.id}`}
          pendingEdit={pendingEdit}
          onAnswer={onAnswerEdit}
        />
      )}
    </>
  );
}
