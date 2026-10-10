// ── surface: shared ──────────────────────────────────
// Thread list + view switch + settings button (avatar only — no cog, by design).
// Exports: Sidebar · Related: hooks/useThreadManager.ts, App.tsx

import type { ThreadInfo } from "../../lib/provider";

function userInitials(name?: string): string {
  // First letter of the first word, plus the first letter of the second word
  // only when there is one: "First" → "F", "First Last" → "FL". Restores the
  // pre-refactor behaviour — the refactored `slice(0, 2)` turned "First" into
  // "FL" (and the unnamed fallback must stay "?", not the cog glyph).
  const n = (name ?? "").trim();
  if (!n) return "?";
  const parts = n.split(/\s+/);
  return (parts[0][0] + (parts[1]?.[0] ?? "")).toUpperCase();
}

export function Sidebar({
  view,
  busy,
  threads,
  currentThreadId,
  menuThreadId,
  menuUp,
  trashed,
  showTrash,
  missingRoots,
  profileName,
  profilePreferred,
  onSwitchView,
  onNewChat,
  onNewSession,
  onSwitchThread,
  onOpenMenu,
  onCloseMenu,
  onRenameThread,
  onDuplicateThread,
  onExportThread,
  onChangeThreadFolder,
  onRemoveThread,
  onToggleShowTrash,
  onRestoreTrash,
  onPurgeTrash,
  onEmptyTrash,
  onOpenSettings,
}: {
  view: "chat" | "code";
  busy: boolean;
  threads: ThreadInfo[];
  currentThreadId: number | null;
  menuThreadId: number | null;
  menuUp: boolean;
  trashed: ThreadInfo[];
  showTrash: boolean;
  missingRoots?: Set<string>;
  profileName: string;
  profilePreferred: string;
  onSwitchView: (v: "chat" | "code") => void;
  onNewChat: () => void;
  onNewSession: () => void;
  onSwitchThread: (id: number) => void;
  onOpenMenu: (id: number, bottom: number) => void;
  onCloseMenu: () => void;
  onRenameThread: (id: number, title: string) => void;
  onDuplicateThread: (id: number) => void;
  onExportThread: (id: number, title: string) => void;
  onChangeThreadFolder?: (id: number) => void;
  onRemoveThread: (id: number) => void;
  onToggleShowTrash: () => void;
  onRestoreTrash: (id: number) => void;
  onPurgeTrash: (id: number, title: string) => void;
  onEmptyTrash: () => void;
  onOpenSettings: () => void;
}) {
  const displayName = profilePreferred || profileName;

  return (
    <aside className="sidebar">
      <div className="section-tabs">
        <button
          className={view === "chat" ? "active" : ""}
          onClick={() => onSwitchView("chat")}
        >
          Chat
        </button>
        <button
          className={view === "code" ? "active" : ""}
          onClick={() => onSwitchView("code")}
        >
          Code
        </button>
      </div>
      <button
        className="new-chat"
        onClick={view === "code" ? onNewSession : onNewChat}
        disabled={busy}
      >
        {view === "code" ? "+ New session" : "+ New chat"}
      </button>
      <div className="thread-list">
        {threads
          .filter((t) => (view === "code" ? !!t.project_root : !t.project_root))
          .map((t) => {
            const isMissing = Boolean(t.project_root && missingRoots?.has(t.project_root));
            return (
            <div
              key={t.id}
              className={`thread ${t.id === currentThreadId ? "active" : ""}`}
              onClick={() => onSwitchThread(t.id)}
            >
              <span className="thread-title">
                {t.title}
                {t.project_root && (
                  <span
                    className={`thread-root${isMissing ? " missing" : ""}`}
                    title={
                      isMissing
                        ? `Project folder not found on disk:\n${t.project_root}\nClick ⋯ to relocate folder`
                        : t.project_root
                    }
                  >
                    {isMissing && "⚠️ "}
                    {t.project_root.split(/[\\/]/).filter(Boolean).pop()}
                  </span>
                )}
              </span>
              <div
                className={`thread-actions${
                  menuThreadId === t.id ? " open" : ""
                }`}
              >
                <button
                  className="thread-menu-btn"
                  title="Options"
                  onClick={(e) => {
                    e.stopPropagation();
                    const r = e.currentTarget.getBoundingClientRect();
                    onOpenMenu(t.id, r.bottom);
                  }}
                >
                  ⋯
                </button>
                {menuThreadId === t.id && (
                  <>
                    <div
                      className="popup-backdrop"
                      onClick={(e) => {
                        e.stopPropagation();
                        onCloseMenu();
                      }}
                    />
                    <div
                      className={`thread-menu${menuUp ? " up" : ""}`}
                      onClick={(e) => e.stopPropagation()}
                      onMouseDown={(e) => e.stopPropagation()}
                    >
                      <button
                        onClick={() => {
                          onCloseMenu();
                          onRenameThread(t.id, t.title);
                        }}
                      >
                        Rename
                      </button>
                      <button
                        onClick={() => {
                          onCloseMenu();
                          onDuplicateThread(t.id);
                        }}
                      >
                        Duplicate
                      </button>
                      <button
                        onClick={() => {
                          onCloseMenu();
                          onExportThread(t.id, t.title);
                        }}
                      >
                        Export
                      </button>
                      {t.project_root && (
                        <>
                          <div className="thread-menu-sep" />
                          <button
                            onClick={() => {
                              onCloseMenu();
                              onChangeThreadFolder?.(t.id);
                            }}
                          >
                            {isMissing ? "Relocate folder…" : "Change folder…"}
                          </button>
                        </>
                      )}
                      <button
                        className="danger"
                        onClick={() => {
                          onCloseMenu();
                          onRemoveThread(t.id);
                        }}
                      >
                        Delete
                      </button>
                    </div>
                  </>
                )}
              </div>
            </div>
            );
          })}
      </div>

      {trashed.length > 0 && (
        <div className="trash">
          <button
            className="trash-head"
            onClick={onToggleShowTrash}
          >
            <span className="trash-chevron">{showTrash ? "▾" : "▸"}</span>
            Recently deleted ({trashed.length})
          </button>
          {showTrash && (
            <div className="trash-list">
              {trashed.map((t) => (
                <div key={t.id} className="trash-item">
                  <span className="trash-title">{t.title}</span>
                  <button
                    className="trash-action"
                    onClick={() => onRestoreTrash(t.id)}
                  >
                    Restore
                  </button>
                  <button
                    className="trash-action danger"
                    title="Delete permanently"
                    onClick={() => onPurgeTrash(t.id, t.title)}
                  >
                    ✕
                  </button>
                </div>
              ))}
              <button className="trash-empty" onClick={onEmptyTrash}>
                Empty trash
              </button>
            </div>
          )}
        </div>
      )}

      <div className="user-row">
        <button
          className="user-bar"
          onClick={onOpenSettings}
          title="Settings"
        >
          <span className="user-avatar">{userInitials(displayName)}</span>
          <span className="user-name">
            {displayName || "Set up profile"}
          </span>
        </button>
      </div>
    </aside>
  );
}
