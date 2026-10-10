// ── surface: shared ──────────────────────────────────
// Diff renderer for approval previews, timeline tool chips, and session file diffs.
// Exports: DiffView · Related: ApprovalOverlay.tsx, ToolChip.tsx, FilesDrawer.tsx

export function DiffView({ diff }: { diff: string }) {
  if (!diff || !diff.trim()) {
    return <div className="diff-empty">No changes</div>;
  }

  return (
    <pre className="diff">
      {diff.split("\n").map((line, i) => {
        let cls = "diff-ctx";
        if (/^--- |^\+\+\+ /.test(line)) cls = "diff-file";
        else if (/^\+(?!\+\+)/.test(line)) cls = "diff-add";
        else if (/^-(?!--)/.test(line)) cls = "diff-del";
        else if (line.startsWith("@@")) cls = "diff-hunk";
        return (
          <div key={i} className={cls}>
            {line || " "}
          </div>
        );
      })}
    </pre>
  );
}
