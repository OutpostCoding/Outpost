// ── surface: shared ──────────────────────────────────
// Trash management — list/restore/purge.
// Exports: TrashModal · Related: useThreadManager.ts

import type { ThreadInfo } from "../lib/provider";

export function TrashModal({
  trashed,
  showTrash,
  onToggleShow,
  onRestore,
  onPurge,
  onEmpty,
}: {
  trashed: ThreadInfo[];
  showTrash: boolean;
  onToggleShow: () => void;
  onRestore: (id: number) => void;
  onPurge: (id: number, title: string) => void;
  onEmpty: () => void;
}) {
  if (trashed.length === 0) return null;

  return (
    <div className="trash">
      <button className="trash-head" onClick={onToggleShow}>
        <span className="trash-chevron">{showTrash ? "▾" : "▸"}</span>
        Recently deleted ({trashed.length})
      </button>
      {showTrash && (
        <div className="trash-list">
          {trashed.map((t) => (
            <div key={t.id} className="trash-item">
              <span className="trash-title">{t.title}</span>
              <button className="trash-action" onClick={() => onRestore(t.id)}>
                Restore
              </button>
              <button
                className="trash-action danger"
                title="Delete permanently"
                onClick={() => onPurge(t.id, t.title)}
              >
                ✕
              </button>
            </div>
          ))}
          <button className="trash-empty" onClick={onEmpty}>
            Empty trash
          </button>
        </div>
      )}
    </div>
  );
}
