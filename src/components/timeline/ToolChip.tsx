// ── surface: chat ──────────────────────────────────
// Tool-call segment chip — renders tool name/status and code cells (incl. coding tools).
// Exports: ToolChip · Related: TurnTimeline.tsx, tools/mod.rs

import { useState } from "react";
import type { Segment } from "../../lib/provider";
import { openPage } from "../../lib/provider";
import { isToolError, STATUS_GLYPH, toolIcon } from "../../utils";
import { CodeCell } from "../ui/CodeCell";
import { DiffView } from "../ui/DiffView";

export function ToolChip({ seg }: { seg: Extract<Segment, { kind: "tool" }> }) {
  const isRunning = seg.output === undefined;
  const hasError = isToolError(seg.output, seg.name);
  const [open, setOpen] = useState(false);
  const [shotOpen, setShotOpen] = useState(false);
  const [opened, setOpened] = useState(false);

  // Extract HTML page path for polite preview button if tool is html_write or html_edit
  let htmlPath: string | null = null;
  if (seg.name === "html_write" || seg.name === "html_edit") {
    if (seg.args) {
      try {
        const parsed = JSON.parse(seg.args);
        if (typeof parsed.path === "string" && parsed.path.trim().length > 0) {
          htmlPath = parsed.path.trim();
        }
      } catch {}
    }
  }

  if (seg.name === "run_python") {
    return <CodeCell code={seg.code ?? ""} output={seg.output ?? ""} />;
  }

  // Build diff or raw arguments preview
  let diffStr: string | null = null;
  let fileContent: string | null = null;
  let codeContent: string | null = null;
  let rawArgs: string | null = null;
  let addedLines = 0;
  let removedLines = 0;

  if (seg.args && seg.args.trim() !== "{}" && seg.args.trim() !== "") {
    try {
      const argsObj = JSON.parse(seg.args);
      if (
        (seg.name === "edit_file" || seg.name === "project_edit") &&
        argsObj.new_string !== undefined
      ) {
        let diff = "";
        if (argsObj.path) {
          diff += `--- ${argsObj.path}\n`;
        }
        if (argsObj.old_string || argsObj.expected) {
          const oldLines = (argsObj.old_string || argsObj.expected).split("\n");
          removedLines += oldLines.length;
          diff += oldLines.map((l: string) => "- " + l).join("\n") + "\n";
        }
        if (argsObj.new_string) {
          const newLines = argsObj.new_string.split("\n");
          addedLines += newLines.length;
          diff += newLines.map((l: string) => "+ " + l).join("\n") + "\n";
        }
        diffStr = diff.trimEnd();
      } else if (seg.name === "project_edit_batch" && Array.isArray(argsObj.edits)) {
        let diff = "";
        const batchPath = typeof argsObj.path === "string" ? argsObj.path : "";
        for (const edit of argsObj.edits) {
          const filePath = edit.path || batchPath;
          if (filePath) {
            diff += `--- ${filePath}\n`;
          }
          if (edit.old_string) {
            const oldLines = edit.old_string.split("\n");
            removedLines += oldLines.length;
            diff += oldLines.map((l: string) => "- " + l).join("\n") + "\n";
          }
          if (edit.new_string) {
            const newLines = edit.new_string.split("\n");
            addedLines += newLines.length;
            diff += newLines.map((l: string) => "+ " + l).join("\n") + "\n";
          }
        }
        diffStr = diff.trimEnd();
      } else if (seg.name === "run_command" || seg.name === "run_background_process" || seg.name === "process_start") {
        const cmd = argsObj.command || argsObj.cmd;
        if (typeof cmd === "string" && cmd.includes("\n")) {
          codeContent = cmd;
        }
      } else if (
        ["write_file", "project_write", "md_write", "html_write"].includes(seg.name) ||
        (typeof argsObj.content === "string" && argsObj.path)
      ) {
        const content = (argsObj.content ?? argsObj.contents ?? argsObj.text ?? argsObj.body ?? argsObj.file_text) as string | undefined;
        if (typeof content === "string") {
          fileContent = content;
          addedLines = content.length > 0 ? content.split("\n").length : 0;
        }
      } else if (
        seg.name === "run_python" ||
        (typeof argsObj.code === "string" && !diffStr && !fileContent)
      ) {
        const code = (argsObj.code ?? argsObj.script ?? argsObj.python) as string | undefined;
        if (typeof code === "string") {
          codeContent = code;
        }
      } else {
        const keys = Object.keys(argsObj);
        const isRedundantSingleArg =
          Boolean(seg.output) &&
          keys.length === 1 &&
          ["query", "url", "path", "scene"].includes(keys[0]);
        if (!isRedundantSingleArg && keys.length > 0) {
          rawArgs = JSON.stringify(argsObj, null, 2);
        }
      }
    } catch {
      rawArgs = seg.args;
    }
  }

  const hasExpandableContent = Boolean(diffStr || fileContent || codeContent || seg.output || rawArgs);

  return (
    <div className={`tool-chip ${hasError ? "has-error" : isRunning ? "is-running" : ""}`}>
      <div className="tool-chip-row">
        <span className="tool-chip-icon">
          {isRunning ? STATUS_GLYPH.running : hasError ? STATUS_GLYPH.error : toolIcon(seg.name)}
        </span>
        <span className="tool-chip-title">{seg.title}</span>
        {addedLines > 0 && <span className="diff-stat-add">+{addedLines}</span>}
        {removedLines > 0 && <span className="diff-stat-remove">-{removedLines}</span>}
        {hasExpandableContent && (
          <button
            type="button"
            className="tool-chip-peek"
            onClick={() => setOpen((o) => !o)}
          >
            {open ? "hide" : isRunning ? "running" : hasError ? "error" : "result"}
          </button>
        )}
        {htmlPath && !isRunning && !hasError && (
          <button
            type="button"
            className="tool-chip-action"
            title={`Open ${htmlPath} in default browser`}
            onClick={(e) => {
              e.stopPropagation();
              openPage(htmlPath).catch(() => {});
              setOpened(true);
              setTimeout(() => setOpened(false), 2000);
            }}
          >
            {opened ? "Opened ↗" : "Open in Browser ↗"}
          </button>
        )}
      </div>
      {hasExpandableContent && open && (
        <div className="tool-chip-content">
          {diffStr && (
            <div className="tool-chip-diff">
              <DiffView diff={diffStr} />
            </div>
          )}
          {fileContent && (
            <div className="tool-chip-file">
              <pre className="tool-chip-code">{fileContent}</pre>
            </div>
          )}
          {codeContent && (
            <div className="tool-chip-file">
              <pre className="tool-chip-code">{codeContent}</pre>
            </div>
          )}
          {rawArgs && <pre className="tool-chip-args">{rawArgs}</pre>}
          {seg.output && (
            (diffStr || fileContent || codeContent) ? (
              <div className={`tool-chip-status-line ${hasError ? "error-text" : ""}`}>
                {seg.output}
              </div>
            ) : (
              <pre className={`tool-chip-out ${hasError ? "error-text" : ""}`}>
                {seg.output}
              </pre>
            )
          )}
        </div>
      )}
      {seg.image && (
        <button
          type="button"
          className={`tool-chip-shot${shotOpen ? " open" : ""}`}
          title={shotOpen ? "Shrink" : "Enlarge"}
          onClick={() => setShotOpen((o) => !o)}
        >
          <img src={seg.image} alt="what the model was shown" />
        </button>
      )}
    </div>
  );
}
