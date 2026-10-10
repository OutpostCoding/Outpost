// ── surface: shared ──────────────────────────────────
// Copy-to-clipboard button.
// Exports: CopyButton · Related: CodeCell.tsx, icons.tsx

import { useState } from "react";
import { CheckIcon, CopyIcon } from "../icons";

export function CopyButton({ text, className }: { text: string; className?: string }) {
  const [done, setDone] = useState(false);
  return (
    <button
      type="button"
      className={`copy-btn${className ? ` ${className}` : ""}`}
      title="Copy"
      aria-label="Copy"
      onClick={() => {
        navigator.clipboard
          .writeText(text)
          .then(() => {
            setDone(true);
            setTimeout(() => setDone(false), 1200);
          })
          .catch(() => {});
      }}
    >
      {done ? <CheckIcon /> : <CopyIcon />}
    </button>
  );
}
