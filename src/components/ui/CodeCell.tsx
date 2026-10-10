// ── surface: shared ──────────────────────────────────
// Code + output cell with copy button (tool results).
// Exports: CodeCell · Related: ToolChip.tsx, ui/CopyButton.tsx

import { CopyButton } from "./CopyButton";

export function CodeCell({ code, output }: { code: string; output: string }) {
  return (
    <div className="code-cell">
      <div className="code-cell-head">
        <span>▶ Python</span>
        <CopyButton text={code} />
      </div>
      <pre className="code-cell-code">{code}</pre>
      {output ? (
        <>
          <div className="code-cell-subhead">
            <span>output</span>
            <CopyButton text={output} />
          </div>
          <pre className="code-cell-out">{output}</pre>
        </>
      ) : null}
    </div>
  );
}
