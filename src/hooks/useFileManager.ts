// ── surface: code ──────────────────────────────────
// Files-drawer domain — open/save notes/pages/docs/compaction, docx export,
// resize, upload routing.
// Exports: useFileManager, FileManagerOptions · Related: FilesDrawer.tsx, App.tsx

import { useState, useEffect, useRef, type Dispatch, type SetStateAction } from "react";
import {
  listNotes,
  listPages,
  openPage,
  readNote,
  listProjectDocs,
  readProjectDoc,
  writeProjectDoc,
  listProjectFiles,
  checkDirectoryExists,
  readProjectFile,
  getSessionModifiedFiles,
  getFileDiff,
  exportMarkdown,
  exportDocx,
  deleteNote,
  renameNote,
  extractDocument,
  type NoteInfo,
  type Compaction,
} from "../lib/provider";

export interface FileManagerOptions {
  sessionRoot: string | null; // FS root; null = chat surface → every FS op bails (drawer inert)
  currentThreadId?: number | null; // active thread id — changes on thread switch
  busy?: boolean; // active turn flag — auto-refresh on turn completion
  compaction: Compaction | null; // current handover note — target of note writes
  setCompaction: Dispatch<SetStateAction<Compaction | null>>; // update after writing the note
  setError: (msg: string | null) => void; // error reporter
  confirmBox: (message: string, title: string) => Promise<boolean>; // confirm for destructive ops (delete/overwrite)
  promptBox: (title: string, initial: string) => Promise<string | null>; // rename prompt
  trackDocument: (path: string, source: "chat" | "added") => void; // record how a doc entered context
  setAdded: Dispatch<SetStateAction<string[]>>; // list setter for added docs
  setMadeInChat: Dispatch<SetStateAction<string[]>>; // list setter for chat-made docs
  addImageFiles: (files: FileList | File[]) => Promise<void>; // route uploaded images into context
}

const DEFAULT_VIEWER_WIDTH = 800;
const DEFAULT_LIST_WIDTH = 360;

function getStoredExpandedDirs(root: string | null): Set<string> {
  const key = `outpost:tree_expanded:${root || "__global__"}`;
  try {
    const raw = localStorage.getItem(key);
    if (raw) {
      const parsed = JSON.parse(raw);
      if (Array.isArray(parsed)) return new Set(parsed);
    }
  } catch {
    /* ignore storage read error */
  }
  return new Set();
}

function saveStoredExpandedDirs(root: string | null, dirs: Set<string>) {
  const key = `outpost:tree_expanded:${root || "__global__"}`;
  try {
    localStorage.setItem(key, JSON.stringify(Array.from(dirs)));
  } catch {
    /* ignore storage write error */
  }
}

export function useFileManager(opts: FileManagerOptions) {
  const {
    sessionRoot,
    currentThreadId,
    busy,
    compaction,
    setCompaction,
    setError,
    confirmBox,
    promptBox,
    trackDocument,
    setAdded,
    setMadeInChat,
    addImageFiles,
  } = opts;

  const [filesOpen, setFilesOpen] = useState(false);
  // Default viewer width is 800px for both code and markdown notes.
  // Resizing during file inspection is temporary; opening any file resets to default.
  const [listWidth, setListWidth] = useState(DEFAULT_LIST_WIDTH);
  const [viewerWidth, setViewerWidth] = useState(DEFAULT_VIEWER_WIDTH);
  const [notes, setNotes] = useState<NoteInfo[]>([]);
  const [pages, setPages] = useState<NoteInfo[]>([]);
  const [viewingNote, setViewingNote] = useState<string | null>(null);
  const [noteContent, setNoteContent] = useState("");
  const [noteEditing, setNoteEditing] = useState(false);
  const [noteDraft, setNoteDraft] = useState("");
  const [noteRaw, setNoteRaw] = useState(false);
  // Display-only metadata for the compaction viewer (fold count + timestamp).
  // Not part of the note text; shown as a readout above the note body.
  const [compactionMeta, setCompactionMeta] = useState<string | null>(null);
  // The viewer shows a saved note/page (chat), a project markdown doc (session),
  // or a compaction summary; the kind decides where an edit saves and which
  // note-only controls apply.
  const [viewerKind, setViewerKind] = useState<
    "note" | "project" | "compaction" | "code"
  >("note");
  // The project's markdown docs, listed for the session-side viewer.
  const [projectDocs, setProjectDocs] = useState<string[]>([]);
  // All project files, listed for the code file tree.
  const [projectFiles, setProjectFiles] = useState<string[]>([]);
  // Expanded directories in the code tree (persisted in localStorage per project).
  const [expandedDirs, setExpandedDirs] = useState<Set<string>>(() =>
    getStoredExpandedDirs(sessionRoot),
  );
  // Files modified, added, or deleted during the current session (for tree dots).
  const [modifiedFiles, setModifiedFiles] = useState<Set<string>>(new Set());
  // Normal vs Diff toggle in the in-drawer file viewer.
  const [showDiff, setShowDiff] = useState(false);
  // Unified diff text for the currently open file (null if unchanged or not loaded).
  const [fileDiff, setFileDiff] = useState<string | null>(null);
  // True if the active session's project folder is missing or deleted from disk.
  const [folderMissing, setFolderMissing] = useState(false);

  // --- Markdown files panel (view + edit notes-folder .md files) -------------

  async function refreshNotes() {
    try {
      setNotes(await listNotes());
    } catch {
      /* ignore — the panel just keeps what it last had */
    }
    try {
      setPages(await listPages());
    } catch {
      /* ignore */
    }
  }

  async function openPageInBrowser(name: string) {
    try {
      await openPage(name);
    } catch (e) {
      setError(`Couldn't open ${name}: ${e}`);
    }
  }

  // View an HTML page's raw source in the viewer (read mode, not rendered).
  async function openPageSource(name: string) {
    refreshNotes();
    try {
      const content = await readNote(name);
      setViewerWidth(DEFAULT_VIEWER_WIDTH);
      setViewerKind("note");
      setCompactionMeta(null);
      setViewingNote(name);
      setNoteContent(content);
      setNoteDraft(content);
      setNoteEditing(false);
      setNoteRaw(true);
      setFilesOpen(true);
    } catch (e) {
      setError(`Couldn't open ${name}: ${e}`);
    }
  }

  function openFiles() {
    setFilesOpen(true);
    // In a session the panel is the project's files/docs; in chat it's the notes.
    if (sessionRoot) {
      refreshProjectDocs();
      refreshProjectFiles();
      refreshModifiedFiles();
    } else {
      refreshNotes();
    }
  }

  async function openNote(name: string) {
    refreshNotes();
    try {
      const content = await readNote(name);
      setViewerWidth(DEFAULT_VIEWER_WIDTH);
      setViewerKind("note");
      setCompactionMeta(null);
      setViewingNote(name);
      setNoteContent(content);
      setNoteDraft(content);
      setNoteEditing(false);
      setNoteRaw(false);
      setFilesOpen(true);
    } catch (e) {
      setError(`Couldn't open ${name}: ${e}`);
    }
  }

  const docsSeq = useRef(0);
  const filesSeq = useRef(0);
  const modSeq = useRef(0);
  const diffSeq = useRef(0);

  // Refresh the project's markdown docs (the session-side file list).
  async function refreshProjectDocs(root = sessionRoot) {
    if (!root) {
      setProjectDocs([]);
      return;
    }
    const seq = ++docsSeq.current;
    try {
      const docs = await listProjectDocs(root);
      if (seq === docsSeq.current) setProjectDocs(docs);
    } catch {
      if (seq === docsSeq.current) setProjectDocs([]);
    }
  }

  // Refresh all project files (for the session-side code file tree).
  async function refreshProjectFiles(root = sessionRoot) {
    if (!root) {
      setProjectFiles([]);
      setFolderMissing(false);
      return;
    }
    const seq = ++filesSeq.current;
    try {
      const exists = await checkDirectoryExists(root);
      if (seq !== filesSeq.current) return;
      if (!exists) {
        setFolderMissing(true);
        setProjectFiles([]);
        setProjectDocs([]);
        setModifiedFiles(new Set());
        closeNote();
        return;
      }
      const files = await listProjectFiles(root);
      if (seq === filesSeq.current) {
        setFolderMissing(false);
        setProjectFiles(files);
      }
    } catch (err: any) {
      if (seq === filesSeq.current) {
        const errStr = String(err || "");
        if (
          errStr.includes("not a directory") ||
          errStr.includes("cannot find") ||
          errStr.includes("No such file")
        ) {
          setFolderMissing(true);
          setProjectFiles([]);
          setProjectDocs([]);
          setModifiedFiles(new Set());
          closeNote();
        }
      }
    }
  }

  // Refresh the list of files modified during this session (for tree modification dots).
  async function refreshModifiedFiles(root = sessionRoot) {
    if (!root) {
      setModifiedFiles(new Set());
      return;
    }
    const seq = ++modSeq.current;
    try {
      const files = await getSessionModifiedFiles(root);
      if (seq === modSeq.current) {
        setModifiedFiles(new Set(files));
        if (files.length > 0) {
          setExpandedDirs((prev) => {
            let changed = false;
            const next = new Set(prev);
            for (const p of files) {
              const parts = p.split("/").filter(Boolean);
              let curr = "";
              for (let i = 0; i < parts.length - 1; i++) {
                curr = curr ? `${curr}/${parts[i]}` : parts[i];
                if (!next.has(curr)) {
                  next.add(curr);
                  changed = true;
                }
              }
            }
            if (changed) {
              saveStoredExpandedDirs(root, next);
              return next;
            }
            return prev;
          });
        }
      }
    } catch {
      if (seq === modSeq.current) setModifiedFiles(new Set());
    }
  }

  // Fetch the unified diff for a specific file against its pre-session snapshot.
  async function loadFileDiff(path: string, root = sessionRoot) {
    if (!root) {
      setFileDiff(null);
      return;
    }
    const seq = ++diffSeq.current;
    try {
      const diff = await getFileDiff(root, path);
      if (seq === diffSeq.current) setFileDiff(diff);
    } catch {
      if (seq === diffSeq.current) setFileDiff(null);
    }
  }

  // Keep the files drawer synchronized with the active session/project.
  // Whenever sessionRoot changes (e.g. clicking through projects in the sidebar),
  // refresh the project files, docs, and modified files immediately, and close any
  // open note from the previous project so the drawer isn't stuck viewing a stale file.
  const prevRootRef = useRef<string | null>(sessionRoot);
  useEffect(() => {
    if (prevRootRef.current !== sessionRoot) {
      closeNote();
      setProjectFiles([]);
      setProjectDocs([]);
      setModifiedFiles(new Set());
      setFolderMissing(false);
      setExpandedDirs(getStoredExpandedDirs(sessionRoot));
      prevRootRef.current = sessionRoot;
    }

    if (sessionRoot) {
      void refreshProjectFiles(sessionRoot);
      void refreshProjectDocs(sessionRoot);
      void refreshModifiedFiles(sessionRoot);
    } else {
      setFolderMissing(false);
      void refreshNotes();
      setModifiedFiles(new Set());
    }
  }, [sessionRoot, currentThreadId]);

  // Window focus: refresh files if the drawer is currently open,
  // catching any external filesystem changes (e.g. git, IDE, external edits).
  useEffect(() => {
    if (!filesOpen) return;
    const onFocus = () => {
      if (sessionRoot) {
        void refreshProjectDocs(sessionRoot);
        void refreshProjectFiles(sessionRoot);
        void refreshModifiedFiles(sessionRoot);
      } else {
        void refreshNotes();
      }
    };
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [filesOpen, sessionRoot]);

  // Turn completion: refresh files when the agent finishes executing tool calls
  // (e.g. project_write, project_edit, md_write).
  const prevBusyRef = useRef(busy);
  useEffect(() => {
    if (prevBusyRef.current && !busy) {
      if (sessionRoot) {
        void refreshModifiedFiles(sessionRoot);
        void refreshProjectDocs(sessionRoot);
        void refreshProjectFiles(sessionRoot);
        if (viewingNote) void loadFileDiff(viewingNote, sessionRoot);
      } else {
        void refreshNotes();
      }
    }
    prevBusyRef.current = busy;
  }, [busy, sessionRoot, viewingNote]);

  function toggleDir(dirPath: string) {
    setExpandedDirs((prev) => {
      const next = new Set(prev);
      if (next.has(dirPath)) next.delete(dirPath);
      else next.add(dirPath);
      saveStoredExpandedDirs(sessionRoot, next);
      return next;
    });
  }

  // Open any project file in the in-drawer code viewer (or markdown viewer if .md).
  async function openProjectFile(path: string) {
    if (!sessionRoot) return;
    const lower = path.toLowerCase();
    if (lower.endsWith(".md") || lower.endsWith(".markdown")) {
      return openProjectDoc(path);
    }
    try {
      const content = await readProjectFile(sessionRoot, path);
      setViewerWidth(DEFAULT_VIEWER_WIDTH);
      setViewerKind("code");
      setCompactionMeta(null);
      setViewingNote(path);
      setNoteContent(content);
      setNoteDraft(content);
      setNoteEditing(false);
      setNoteRaw(false);
      setShowDiff(false);
      setFileDiff(null);
      setFilesOpen(true);
      void loadFileDiff(path, sessionRoot);
    } catch (e) {
      setError(`Couldn't open ${path}: ${e}`);
    }
  }

  // Open one of the project's markdown docs in the viewer — read + edit, saving
  // straight back to the file (the human is the authority; the model's
  // project_write stays approval-gated).
  async function openProjectDoc(path: string) {
    if (!sessionRoot) return;
    try {
      const content = await readProjectDoc(sessionRoot, path);
      setViewerWidth(DEFAULT_VIEWER_WIDTH);
      setViewerKind("project");
      setCompactionMeta(null);
      setViewingNote(path);
      setNoteContent(content);
      setNoteDraft(content);
      setNoteEditing(false);
      setNoteRaw(false);
      setShowDiff(false);
      setFileDiff(null);
      setFilesOpen(true);
      void loadFileDiff(path, sessionRoot);
    } catch (e) {
      setError(`Couldn't open ${path}: ${e}`);
    }
  }

  // Open the compaction handover note in the viewer — read + edit. The note is
  // frontend state (not a file), so saving writes straight back to `compaction`.
  // This makes the note visible and editable: the user can trim, fix a wrong file
  // path, or annotate it, and the next turn sends the edited version.
  function openCompaction() {
    if (!compaction) return;
    refreshProjectDocs(); // so "back" from the note lands on the project files
    refreshProjectFiles();
    setViewerWidth(DEFAULT_VIEWER_WIDTH);
    setViewerKind("compaction");
    const folded = compaction.up_to;
    const when = new Date(compaction.at).toLocaleString();
    setViewingNote("Compaction");
    setNoteContent(compaction.summary);
    setNoteDraft(compaction.summary);
    setNoteEditing(false);
    setNoteRaw(false);
    setShowDiff(false);
    setFileDiff(null);
    setFilesOpen(true);
    // Stash the metadata for the readout above the note (not part of the note
    // itself — it's display-only context).
    setCompactionMeta(
      folded > 0
        ? `Folded ${folded} messages · ${when}`
        : `Standing handover note · ${when}`,
    );
  }

  function closeNote() {
    setViewingNote(null);
    setNoteEditing(false);
    setShowDiff(false);
    setFileDiff(null);
    setViewerKind("note");
    setCompactionMeta(null);
    setViewerWidth(DEFAULT_VIEWER_WIDTH);
  }

  function startNoteEdit() {
    setNoteDraft(noteContent);
    setNoteRaw(false);
    setShowDiff(false);
    setNoteEditing(true);
  }

  async function saveNote() {
    if (viewingNote == null) return;
    try {
      if (viewerKind === "compaction") {
        // The compaction note is frontend state, not a file. Save straight back
        // to the Compaction object — up_to and at are unchanged (the fold
        // boundary didn't move, only the note text did).
        if (!compaction) return;
        setCompaction({ ...compaction, summary: noteDraft });
      } else if (viewerKind === "project") {
        if (!sessionRoot) return;
        await writeProjectDoc(sessionRoot, viewingNote, noteDraft);
        void refreshModifiedFiles(sessionRoot);
        void loadFileDiff(viewingNote, sessionRoot);
      } else {
        await exportMarkdown(viewingNote, noteDraft);
        refreshNotes();
      }
      setNoteContent(noteDraft);
      setNoteEditing(false);
    } catch (e) {
      setError(`Save failed: ${e}`);
    }
  }

  // Deterministic export of the viewed markdown note to a Word .docx via a native
  // Save dialog (the user names + places the file), which then opens. No model
  // involved — the same generation engine, invoked directly by the user.
  async function exportNoteDocx() {
    if (viewingNote == null) return;
    const base = viewingNote.replace(/\.[^./\\]+$/, "") || "document";
    try {
      await exportDocx(noteEditing ? noteDraft : noteContent, `${base}.docx`);
    } catch (e) {
      setError(`Export failed: ${e}`);
    }
  }

  async function deleteNoteByName(name: string) {
    const ok = await confirmBox(
      `Delete "${name}"? This removes the file from the notes folder.`,
      "Delete file",
    );
    if (!ok) return;
    try {
      await deleteNote(name);
      setAdded((a) => a.filter((p) => p !== name));
      setMadeInChat((m) => m.filter((p) => p !== name));
      if (viewingNote === name) closeNote();
      refreshNotes();
    } catch (e) {
      setError(`Delete failed: ${e}`);
    }
  }

  async function renameNoteFromViewer(name: string) {
    const next = await promptBox("Rename file to:", name);
    if (next == null) return;
    const trimmed = next.trim();
    if (!trimmed || trimmed === name) return;
    try {
      const finalName = await renameNote(name, trimmed);
      setAdded((a) => a.map((p) => (p === name ? finalName : p)));
      setMadeInChat((m) => m.map((p) => (p === name ? finalName : p)));
      setViewingNote(finalName);
      refreshNotes();
    } catch (e) {
      setError(`Rename failed: ${e}`);
    }
  }

  const filesWidth = viewingNote ? viewerWidth : listWidth;

  // Drag the left edge of the Files panel to resize it.
  function startFilesResize(e: { clientX: number; preventDefault: () => void }) {
    e.preventDefault();
    const startX = e.clientX;
    // Resize whichever width is currently showing: file tree list vs active viewer.
    const viewing = viewingNote != null;
    const startW = viewing ? viewerWidth : listWidth;
    const setW = viewing ? setViewerWidth : setListWidth;
    const onMove = (ev: MouseEvent) => {
      const w = Math.min(
        Math.max(startW + (startX - ev.clientX), 320),
        Math.round(window.innerWidth * 0.72),
      );
      setW(w);
    };
    const onUp = () => {
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  }

  // Upload external markdown/text file(s) into the notes folder, then add them to
  // the chat as "Added" documents (switched on in context), or attach image(s) for vision models.
  async function handleUpload(e: { target: HTMLInputElement }) {
    const input = e.target;
    const files = input.files ? Array.from(input.files) : [];
    input.value = ""; // allow re-selecting the same file later
    if (files.length === 0) return;
    const imageFiles: File[] = [];
    for (const file of files) {
      if (file.type.startsWith("image/")) {
        imageFiles.push(file);
        continue;
      }
      try {
        const lower = file.name.toLowerCase();
        let text: string;
        if (lower.endsWith(".docx") || lower.endsWith(".pdf")) {
          // Word/PDF: the Rust backend extracts markdown/text from the raw bytes;
          // from here it's saved and read exactly like a .md/.txt upload.
          const bytes = Array.from(new Uint8Array(await file.arrayBuffer()));
          text = await extractDocument(file.name, bytes);
        } else {
          text = await file.text();
        }
        const base = file.name.replace(/\.[^./\\]+$/, "") || "upload";
        const name = `${base}.md`;
        await exportMarkdown(name, text);
        trackDocument(name, "added");
        await refreshNotes();
        openNote(name);
        setFilesOpen(true);
      } catch (err) {
        setError(`Upload failed: ${err}`);
      }
    }
    if (imageFiles.length > 0) {
      addImageFiles(imageFiles);
    }
  }

  return {
    filesOpen,
    setFilesOpen,
    listWidth,
    setListWidth,
    viewerWidth,
    setViewerWidth,
    filesWidth,
    notes,
    setNotes,
    pages,
    setPages,
    viewingNote,
    setViewingNote,
    noteContent,
    setNoteContent,
    noteEditing,
    setNoteEditing,
    noteDraft,
    setNoteDraft,
    noteRaw,
    setNoteRaw,
    compactionMeta,
    setCompactionMeta,
    viewerKind,
    setViewerKind,
    projectDocs,
    setProjectDocs,
    projectFiles,
    setProjectFiles,
    expandedDirs,
    setExpandedDirs,
    toggleDir,
    modifiedFiles,
    showDiff,
    setShowDiff,
    fileDiff,
    loadFileDiff,
    folderMissing,
    setFolderMissing,
    refreshModifiedFiles,
    openProjectFile,
    refreshProjectFiles,
    refreshNotes,
    openPageInBrowser,
    openPageSource,
    openFiles,
    openNote,
    refreshProjectDocs,
    openProjectDoc,
    openCompaction,
    closeNote,
    startNoteEdit,
    saveNote,
    exportNoteDocx,
    deleteNoteByName,
    renameNoteFromViewer,
    startFilesResize,
    handleUpload,
  };
}