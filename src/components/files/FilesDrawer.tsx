// ── surface: code ──────────────────────────────────
// Draggable files drawer — notes/pages/docs/compaction; render only (state in the hook).
// Exports: FilesDrawer · Related: useFileManager.ts, App.tsx, AppContext.tsx

import { useMemo, useState, useEffect } from "react";
import {
  FolderIcon,
  FilesIcon,
  ChevronRightIcon,
  ChevronDownIcon,
  TrashIcon,
  SearchIcon,
  ExternalLinkIcon,
} from "../icons";
import { CopyButton } from "../ui/CopyButton";
import { DiffView } from "../ui/DiffView";
import { Markdown } from "../markdown/Markdown";
import { useAppContext } from "../../AppContext";
import { formatTime } from "../../utils";
import { revealNote, revealFolder, type NoteInfo } from "../../lib/provider";
import { highlightCode, escapeHtml } from "./codeHighlight";

interface FileTreeNode {
  name: string;
  path: string;
  isDir: boolean;
  children: FileTreeNode[];
}

function buildFileTree(paths: string[]): FileTreeNode[] {
  const rootObj: Record<string, any> = {};

  for (const p of paths) {
    const parts = p.split("/").filter(Boolean);
    let curr = rootObj;
    let currPath = "";

    for (let i = 0; i < parts.length; i++) {
      const part = parts[i];
      currPath = currPath ? `${currPath}/${part}` : part;
      const isFile = i === parts.length - 1;

      if (!curr[part]) {
        curr[part] = {
          __name: part,
          __path: currPath,
          __isDir: !isFile,
          __children: {},
        };
      }
      curr = curr[part].__children;
    }
  }

  function toNodes(obj: Record<string, any>): FileTreeNode[] {
    const nodes: FileTreeNode[] = Object.values(obj).map((entry: any) => ({
      name: entry.__name,
      path: entry.__path,
      isDir: entry.__isDir,
      children: toNodes(entry.__children),
    }));

    return nodes.sort((a, b) => {
      if (a.isDir !== b.isDir) {
        return a.isDir ? -1 : 1;
      }
      return a.name.localeCompare(b.name, undefined, { sensitivity: "base" });
    });
  }

  return toNodes(rootObj);
}

function flattenTree(
  nodes: FileTreeNode[],
  expandedDirs: Set<string>,
  depth = 0,
): { node: FileTreeNode; depth: number }[] {
  const result: { node: FileTreeNode; depth: number }[] = [];
  for (const node of nodes) {
    result.push({ node, depth });
    if (node.isDir && expandedDirs.has(node.path)) {
      result.push(...flattenTree(node.children, expandedDirs, depth + 1));
    }
  }
  return result;
}

export function FilesDrawer({
  filesOpen,
  filesWidth,
  onClose,
  onStartResize,
}: {
  filesOpen: boolean;
  filesWidth: number;
  onClose: () => void;
  onStartResize: (e: React.MouseEvent) => void;
}) {
  const {
    viewingNote,
    viewerKind,
    noteContent,
    noteEditing,
    noteDraft,
    noteRaw,
    compactionMeta,
    isStaged,
    onCloseNote,
    onRenameNote,
    onRevealNote,
    onDeleteNote,
    toggleStagedFile,
    onStartNoteEdit,
    onSaveNote,
    onExportDocx,
    setNoteEditing,
    setNoteRaw,
    setNoteDraft,
    sessionRoot,
    notes,
    pages,
    projectFiles,
    expandedDirs,
    toggleDir,
    modifiedFiles,
    showDiff,
    setShowDiff,
    fileDiff,
    loadFileDiff,
    added,
    madeInChat,
    compaction,
    openCompaction,
    openProjectFile,
    openNote,
    openPageInBrowser,
    openPageSource,
    deleteNoteByName,
    folderMissing,
    onChangeSessionRoot,
    onRevealFolder,
  } = useAppContext();

  const [filterQuery, setFilterQuery] = useState("");
  const [filterModifiedOnly, setFilterModifiedOnly] = useState(false);
  const [filterCollapsedDirs, setFilterCollapsedDirs] = useState<Set<string>>(
    () => new Set(),
  );

  // Reset filter state when session project changes
  useEffect(() => {
    setFilterQuery("");
    setFilterModifiedOnly(false);
    setFilterCollapsedDirs(new Set());
  }, [sessionRoot]);

  // Reset directory collapse overrides whenever filter parameters change
  useEffect(() => {
    setFilterCollapsedDirs(new Set());
  }, [filterQuery, filterModifiedOnly]);

  // If there are no modified files, ensure modified-only mode reverts to all files
  useEffect(() => {
    if ((!modifiedFiles || modifiedFiles.size === 0) && filterModifiedOnly) {
      setFilterModifiedOnly(false);
    }
  }, [modifiedFiles, filterModifiedOnly]);

  const isFiltering = filterQuery.trim().length > 0 || filterModifiedOnly;
  const modifiedCount = modifiedFiles ? modifiedFiles.size : 0;

  const filteredFiles = useMemo(() => {
    let files = projectFiles;
    if (filterModifiedOnly) {
      files = files.filter((p) => modifiedFiles?.has(p));
    }
    const q = filterQuery.trim().toLowerCase();
    if (q) {
      files = files.filter((p) => p.toLowerCase().includes(q));
    }
    return files;
  }, [projectFiles, filterModifiedOnly, filterQuery, modifiedFiles]);

  const treeNodes = useMemo(() => buildFileTree(filteredFiles), [filteredFiles]);

  const effectiveExpandedDirs = useMemo(() => {
    if (!isFiltering) return expandedDirs;
    // When filtering, auto-expand all parent folders containing matches,
    // respecting any manual collapse toggles within the filtered view.
    const dirs = new Set<string>();
    for (const p of filteredFiles) {
      const parts = p.split("/").filter(Boolean);
      let curr = "";
      for (let i = 0; i < parts.length - 1; i++) {
        curr = curr ? `${curr}/${parts[i]}` : parts[i];
        if (!filterCollapsedDirs.has(curr)) {
          dirs.add(curr);
        }
      }
    }
    return dirs;
  }, [isFiltering, expandedDirs, filteredFiles, filterCollapsedDirs]);

  const visibleNodes = useMemo(
    () => flattenTree(treeNodes, effectiveExpandedDirs),
    [treeNodes, effectiveExpandedDirs],
  );

  const handleToggleDir = (dirPath: string) => {
    if (isFiltering) {
      setFilterCollapsedDirs((prev) => {
        const next = new Set(prev);
        if (next.has(dirPath)) {
          next.delete(dirPath);
        } else {
          next.add(dirPath);
        }
        return next;
      });
    } else {
      toggleDir(dirPath);
    }
  };

  const codeHighlightResult = useMemo(() => {
    if (viewerKind !== "code" || !viewingNote) return null;
    return highlightCode(noteContent, viewingNote);
  }, [viewerKind, viewingNote, noteContent]);

  if (!filesOpen) return null;

  function renderProjectFiles() {
    if (folderMissing) {
      return (
        <div className="files-folder-missing">
          <div className="folder-missing-icon" aria-hidden="true">
            <FolderIcon />
          </div>
          <div className="folder-missing-title">Project folder not found</div>
          <div className="folder-missing-path" title={sessionRoot ?? ""}>
            {sessionRoot}
          </div>
          <div className="folder-missing-desc">
            This folder may have been moved, renamed, or deleted from disk.
          </div>
          {onChangeSessionRoot && (
            <button
              type="button"
              className="folder-missing-btn"
              onClick={onChangeSessionRoot}
            >
              Relocate folder…
            </button>
          )}
        </div>
      );
    }
    return (
      <>
        {compaction && (
          <div className="files-section" key="__compaction">
            <div className="files-section-label">Session</div>
            <div className="file-item">
              <button className="file-open" onClick={openCompaction}>
                <span className="file-name">✎ Compaction</span>
              </button>
            </div>
          </div>
        )}
        <div className="files-section" key="__projectfiles">
          <div className="files-section-header">
            <div className="files-section-label">
              Files
              {projectFiles.length > 0 && (
                <span className="files-count-badge">({projectFiles.length})</span>
              )}
            </div>
            {modifiedCount > 0 && (
              <div className="files-filter-pills">
                <button
                  type="button"
                  className={`files-pill${!filterModifiedOnly ? " active" : ""}`}
                  onClick={() => setFilterModifiedOnly(false)}
                  title="Show all project files"
                >
                  All
                </button>
                <button
                  type="button"
                  className={`files-pill${filterModifiedOnly ? " active" : ""}`}
                  onClick={() => setFilterModifiedOnly(true)}
                  title="Show only files modified during this session"
                >
                  <span className="tree-mod-dot inline" />
                  Modified ({modifiedCount})
                </button>
              </div>
            )}
          </div>

          {projectFiles.length > 0 && (
            <div className="tree-filter-bar">
              <span className="tree-filter-icon">
                <SearchIcon />
              </span>
              <input
                type="text"
                className="tree-filter-input"
                placeholder={filterModifiedOnly ? "Filter modified files..." : "Filter files..."}
                value={filterQuery}
                onChange={(e) => setFilterQuery(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Escape") {
                    setFilterQuery("");
                  }
                }}
                spellCheck={false}
              />
              {filterQuery && (
                <div className="tree-filter-actions">
                  <span className="tree-filter-count">{filteredFiles.length}</span>
                  <button
                    type="button"
                    className="tree-filter-clear"
                    onClick={() => setFilterQuery("")}
                    title="Clear filter (Esc)"
                  >
                    ✕
                  </button>
                </div>
              )}
            </div>
          )}

          {filteredFiles.length === 0 ? (
            <div className="files-empty">
              {filterQuery && filterModifiedOnly
                ? `No modified files match "${filterQuery}".`
                : filterQuery
                ? `No files match "${filterQuery}".`
                : filterModifiedOnly
                ? "No files modified in this session."
                : "No files in this project."}
            </div>
          ) : (
            <div className="tree-root">
              {visibleNodes.map(({ node, depth }) => {
                if (node.isDir) {
                  const expanded = effectiveExpandedDirs.has(node.path);
                  const hasModifiedChildren =
                    !expanded &&
                    Array.from(modifiedFiles || []).some((p) =>
                      p.startsWith(node.path + "/"),
                    );
                  return (
                    <div
                      key={node.path}
                      className="tree-item tree-dir"
                      style={{ paddingLeft: `${8 + depth * 14}px` }}
                      onClick={() => handleToggleDir(node.path)}
                      title={node.path}
                    >
                      <span className="tree-caret">
                        {expanded ? <ChevronDownIcon /> : <ChevronRightIcon />}
                      </span>
                      <span className="tree-icon">
                        <FolderIcon />
                      </span>
                      <span className="tree-name">{node.name}</span>
                      {hasModifiedChildren && (
                        <span
                          className="tree-dir-mod-dot"
                          title="Contains modified files"
                        />
                      )}
                    </div>
                  );
                }
                const active = viewingNote === node.path;
                const isModified = Boolean(modifiedFiles?.has(node.path));
                return (
                  <div
                    key={node.path}
                    className={`tree-item tree-file${active ? " active" : ""}`}
                    style={{ paddingLeft: `${8 + depth * 14 + 16}px` }}
                    draggable={true}
                    onDragStart={(e) => {
                      e.dataTransfer.setData("application/x-outpost-file", node.path);
                      e.dataTransfer.setData("text/plain", node.path);
                      e.dataTransfer.effectAllowed = "copy";
                    }}
                  >
                    <button
                      className="tree-file-btn"
                      onClick={() => openProjectFile(node.path)}
                      title={node.path}
                    >
                      <span className="tree-icon">
                        <FilesIcon />
                      </span>
                      <span className="tree-name">{node.name}</span>
                      {isModified && (
                        <span
                          className="tree-mod-dot"
                          title="Modified this session"
                        />
                      )}
                    </button>
                  </div>
                );
              })}
            </div>
          )}
        </div>
      </>
    );
  }

  function renderFiles() {
    const addedNotes = notes.filter((n) => added.includes(n.name));
    const chatNotes = notes.filter(
      (n) => madeInChat.includes(n.name) && !added.includes(n.name),
    );
    const otherNotes = notes.filter(
      (n) => !added.includes(n.name) && !madeInChat.includes(n.name),
    );
    const section = (label: string, items: NoteInfo[]) =>
      items.length === 0 ? null : (
        <div className="files-section" key={label}>
          <div className="files-section-label">{label}</div>
          {items.map((n) => (
            <div
              key={n.name}
              className="file-item"
              draggable={true}
              onDragStart={(e) => {
                e.dataTransfer.setData("application/x-outpost-file", n.name);
                e.dataTransfer.setData("text/plain", n.name);
                e.dataTransfer.effectAllowed = "copy";
              }}
            >
              <button className="file-open" onClick={() => openNote(n.name)}>
                <span className="file-name">{n.name}</span>
                <span className="file-time">{formatTime(n.modified)}</span>
              </button>
              <button
                className="file-del"
                onClick={() => deleteNoteByName(n.name)}
                title="Delete file"
                aria-label="Delete file"
              >
                <TrashIcon />
              </button>
            </div>
          ))}
        </div>
      );
    const pagesSection =
      pages.length === 0 ? null : (
        <div className="files-section" key="__pages">
          <div className="files-section-label">Pages</div>
          {pages.map((p) => (
            <div key={p.name} className="file-item">
              <button
                className="file-open"
                onClick={() => openPageInBrowser(p.name)}
                title="Open in browser"
              >
                <span className="file-name">{p.name}</span>
                <span className="file-time">{formatTime(p.modified)}</span>
              </button>
              <button
                className="file-del"
                onClick={() => openPageSource(p.name)}
                title="View source"
                aria-label="View source"
              >
                <span className="file-src">{"</>"}</span>
              </button>
              <button
                className="file-del"
                onClick={() => revealNote(p.name).catch(() => {})}
                title="Show in folder"
                aria-label="Show in folder"
              >
                <FolderIcon />
              </button>
              <button
                className="file-del"
                onClick={() => deleteNoteByName(p.name)}
                title="Delete file"
                aria-label="Delete file"
              >
                <TrashIcon />
              </button>
            </div>
          ))}
        </div>
      );
    return (
      <>
        {/* Chat side gets the same Compaction entry the session list has: the
            handover note is thread state, not a file — this is the only place
            to review and edit it before it keeps being sent to the model. */}
        {compaction && (
          <div className="files-section" key="__compaction">
            <div className="files-section-label">Session</div>
            <div className="file-item">
              <button className="file-open" onClick={openCompaction}>
                <span className="file-name">✎ Compaction</span>
              </button>
            </div>
          </div>
        )}
        {section("Added", addedNotes)}
        {section("Made in chat", chatNotes)}
        {section("Other", otherNotes)}
        {pagesSection}
      </>
    );
  }

  return (
    <aside
      className="files-panel"
      style={{ width: filesWidth, flexBasis: filesWidth }}
    >
      <div className="files-resize-handle" onMouseDown={onStartResize} />
      <div className="files-head">
        {viewingNote ? (
          <>
            <button
              className="files-back"
              onClick={onCloseNote}
              title="Back to files"
            >
              ‹
            </button>
            {viewerKind === "note" ? (
              <button
                className="files-title files-rename"
                onClick={() => onRenameNote(viewingNote)}
                title="Click to rename"
              >
                {viewingNote}
              </button>
            ) : (
              <span className="files-title">{viewingNote}</span>
            )}
          </>
        ) : (
          <div className="files-title-row">
            <span className="files-title">Files</span>
            {sessionRoot && (
              <>
                <button
                  className={`files-root-badge${folderMissing ? " missing" : ""}`}
                  onClick={onChangeSessionRoot}
                  title={
                    folderMissing
                      ? `Project folder not found:\n${sessionRoot}\nClick to relocate folder`
                      : `Project folder: ${sessionRoot}\nClick to change folder`
                  }
                >
                  <FolderIcon />
                  <span className="files-root-name">
                    {sessionRoot.split(/[\\/]/).filter(Boolean).pop()}
                  </span>
                  {folderMissing ? (
                    <span className="files-root-missing-tag">missing</span>
                  ) : (
                    <span className="files-root-edit">✎</span>
                  )}
                </button>
                {!folderMissing && (
                  <button
                    className="files-icon-btn files-root-reveal-btn"
                    onClick={() => {
                      if (onRevealFolder) {
                        onRevealFolder(sessionRoot);
                      } else {
                        revealFolder(sessionRoot).catch(console.error);
                      }
                    }}
                    title={`Open in Explorer\n${sessionRoot}`}
                    aria-label="Open project folder in file manager"
                  >
                    <ExternalLinkIcon />
                  </button>
                )}
              </>
            )}
          </div>
        )}
        {viewingNote && viewerKind === "note" && (
          <button
            className="files-icon-btn"
            onClick={() => onRevealNote(viewingNote)}
            title="Show in file explorer"
          >
            <FolderIcon />
          </button>
        )}
        {viewingNote && viewerKind === "note" && (
          <button
            className="files-icon-btn"
            onClick={() => onDeleteNote(viewingNote)}
            title="Delete file"
          >
            <TrashIcon />
          </button>
        )}
        {viewingNote &&
          viewerKind !== "compaction" && (
            <button
              className={`files-ctx${isStaged(viewingNote) ? " on" : ""}`}
              onClick={() => toggleStagedFile(viewingNote)}
              title={
                isStaged(viewingNote)
                  ? "Staged for next turn — click to unstage"
                  : "Stage this file for the next prompt"
              }
            >
              {isStaged(viewingNote) ? "✓ Staged for turn" : "+ Add to prompt"}
            </button>
          )}
        <button className="files-close" onClick={onClose} title="Close">
          ×
        </button>
      </div>
      {viewingNote ? (
        viewerKind === "code" ? (
          <>
            <div className="files-tabs code-viewer-toolbar">
              <div className="code-view-toggle">
                <button
                  className={!showDiff ? "active" : ""}
                  onClick={() => setShowDiff(false)}
                >
                  Code
                </button>
                <button
                  className={showDiff ? "active" : ""}
                  onClick={() => {
                    setShowDiff(true);
                    if (viewingNote && fileDiff === null) {
                      void loadFileDiff(viewingNote);
                    }
                  }}
                >
                  Diff
                  {viewingNote && Boolean(modifiedFiles?.has(viewingNote)) && (
                    <span className="tab-mod-dot" />
                  )}
                </button>
              </div>
              <span className="code-viewer-lines">
                {!showDiff ? (
                  <>
                    {codeHighlightResult
                      ? codeHighlightResult.lines.length
                      : noteContent.split("\n").length}{" "}
                    lines
                    {codeHighlightResult?.language && (
                      <span className="code-viewer-lang">
                        {" "}
                        · {codeHighlightResult.language}
                      </span>
                    )}
                  </>
                ) : (
                  <span className="code-viewer-lang">Session diff</span>
                )}
              </span>
              <span className="files-tabs-spacer" />
              <CopyButton text={showDiff && fileDiff ? fileDiff : noteContent} />
            </div>
            <div className="files-body code-viewer-body">
              {showDiff ? (
                fileDiff ? (
                  <div className="code-diff-wrap">
                    <DiffView diff={fileDiff} />
                  </div>
                ) : (
                  <div className="files-empty" style={{ padding: "30px 20px" }}>
                    No changes to this file during this session.
                  </div>
                )
              ) : (
                <div className="code-viewer-wrap">
                  <table className="code-viewer-table">
                    <tbody>
                      {(codeHighlightResult
                        ? codeHighlightResult.lines
                        : noteContent.split("\n").map(escapeHtml)
                      ).map((lineHtml, idx) => (
                        <tr key={idx} className="code-viewer-row">
                          <td className="code-line-num">{idx + 1}</td>
                          <td
                            className="code-line-text"
                            dangerouslySetInnerHTML={{
                              __html: lineHtml || "&nbsp;",
                            }}
                          />
                        </tr>
                      ))}
                      <tr className="code-viewer-filler-row" aria-hidden="true">
                        <td className="code-line-num" />
                        <td className="code-line-text" />
                      </tr>
                    </tbody>
                  </table>
                </div>
              )}
            </div>
          </>
        ) : (
          <>
            <div className="files-tabs">
              <button
                className={!noteEditing && !noteRaw && !showDiff ? "active" : ""}
                onClick={() => {
                  setNoteEditing(false);
                  setNoteRaw(false);
                  setShowDiff(false);
                }}
              >
                Rendered
              </button>
              <button
                className={!noteEditing && noteRaw && !showDiff ? "active" : ""}
                onClick={() => {
                  setNoteEditing(false);
                  setNoteRaw(true);
                  setShowDiff(false);
                }}
              >
                Raw
              </button>
              {viewerKind === "project" && (
                <button
                  className={showDiff ? "active" : ""}
                  onClick={() => {
                    setNoteEditing(false);
                    setNoteRaw(false);
                    setShowDiff(true);
                    if (viewingNote && fileDiff === null) {
                      void loadFileDiff(viewingNote);
                    }
                  }}
                >
                  Diff
                  {viewingNote && Boolean(modifiedFiles?.has(viewingNote)) && (
                    <span className="tab-mod-dot" />
                  )}
                </button>
              )}
              <button
                className={noteEditing ? "active" : ""}
                onClick={() => {
                  setShowDiff(false);
                  onStartNoteEdit();
                }}
              >
                Edit
              </button>
              <span className="files-tabs-spacer" />
              {viewerKind === "note" && (
                <button
                  className="files-save"
                  onClick={onExportDocx}
                  title="Export as a Word document (.docx)"
                >
                  Export
                </button>
              )}
              {noteEditing && (
                <button
                  className="files-save"
                  onClick={onSaveNote}
                  disabled={noteDraft === noteContent}
                >
                  Save
                </button>
              )}
            </div>
            {viewerKind === "compaction" && compactionMeta && (
              <div className="files-meta">{compactionMeta}</div>
            )}
            <div className={`files-body${showDiff ? " code-viewer-body" : ""}`}>
              {showDiff ? (
                fileDiff ? (
                  <div className="code-diff-wrap">
                    <DiffView diff={fileDiff} />
                  </div>
                ) : (
                  <div className="files-empty" style={{ padding: "30px 20px" }}>
                    No changes to this file during this session.
                  </div>
                )
              ) : noteEditing ? (
                <textarea
                  className="files-editor"
                  value={noteDraft}
                  onChange={(e) => setNoteDraft(e.target.value)}
                  spellCheck={false}
                />
              ) : noteRaw ? (
                <pre className="export-raw">{noteContent}</pre>
              ) : (
                <Markdown text={noteContent} />
              )}
            </div>
          </>
        )
      ) : (
        <div className="files-list">
          {sessionRoot ? (
            renderProjectFiles()
          ) : notes.length === 0 && pages.length === 0 && !compaction ? (
            <div className="files-empty">
              No files yet. Notes and pages you save or approve show up here.
            </div>
          ) : (
            renderFiles()
          )}
        </div>
      )}
    </aside>
  );
}
