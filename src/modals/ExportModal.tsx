// ── surface: shared ──────────────────────────────────
// Export thread/message picker (markdown / docx).
// Exports: ExportModal · Related: useThreadManager.ts (doExport), docgen.rs

import { Markdown } from "../components/markdown/Markdown";
import { useBackdropClose } from "../hooks/useBackdropClose";

export function ExportModal({
  exportDoc,
  exportName,
  exportSaved,
  exportRaw,
  setExportName,
  setExportRaw,
  onClose,
  onExport,
}: {
  exportDoc: string | null;
  exportName: string;
  exportSaved: string | null;
  exportRaw: boolean;
  setExportName: (name: string) => void;
  setExportRaw: (raw: boolean) => void;
  onClose: () => void;
  onExport: () => void;
}) {
  const backdropHandlers = useBackdropClose(onClose);

  if (exportDoc == null) return null;

  return (
    <div className="export-overlay" {...backdropHandlers}>
      <div
        className="export"
        onClick={(e) => e.stopPropagation()}
        onMouseDown={(e) => e.stopPropagation()}
      >
        <div className="export-head">
          <h3>Export</h3>
          <input
            className="export-name"
            value={exportName}
            onChange={(e) => setExportName(e.target.value)}
            spellCheck={false}
          />
          <div className="export-views">
            <button
              className={exportRaw ? "" : "active"}
              onClick={() => setExportRaw(false)}
            >
              Rendered
            </button>
            <button
              className={exportRaw ? "active" : ""}
              onClick={() => setExportRaw(true)}
            >
              Raw
            </button>
          </div>
        </div>
        <div className="export-preview">
          {exportRaw ? (
            <pre className="export-raw">{exportDoc}</pre>
          ) : (
            <Markdown text={exportDoc} />
          )}
        </div>
        <div className="export-foot">
          <span className="export-saved">
            {exportSaved ? `Saved to ${exportSaved}` : ""}
          </span>
          <button
            onClick={() => {
              if (exportDoc)
                navigator.clipboard.writeText(exportDoc).catch(() => {});
            }}
          >
            Copy
          </button>
          <button onClick={onClose}>
            {exportSaved ? "Done" : "Cancel"}
          </button>
          {!exportSaved && (
            <button className="primary" onClick={onExport}>
              Save to notes
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
