// ── surface: shared ──────────────────────────────────
// Composition root + turn-engine hub — wires the hooks, owns enabledToolList, runTurn,
// slash commands, and the JSX. Deliberately NOT further split.
// Exports: App (default) · Related: hooks/*, ChatSurface.tsx, modals/*

import { useEffect, useRef, useState, useCallback } from "react";
import { useThreadManager } from "./hooks/useThreadManager";
import { useTurnRunner } from "./hooks/useTurnRunner";
import { useModelConfig } from "./hooks/useModelConfig";
import { useFileManager } from "./hooks/useFileManager";
import { useCompactionManager } from "./hooks/useCompactionManager";

import { ExportModal } from "./modals/ExportModal";
import { AppContext, type ExternalFileAttachment } from "./AppContext";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { SettingsModal } from "./modals/SettingsModal";
import { InstructionsModal } from "./modals/InstructionsModal";
import { ContextInspectorModal } from "./modals/ContextInspectorModal";
import { PromptModal } from "./modals/PromptModal";
import { Sidebar } from "./components/sidebar/Sidebar";
import { FilesDrawer } from "./components/files/FilesDrawer";
import { ChatSurface } from "./components/surface/ChatSurface";
import { TerminalPane } from "./components/surface/TerminalPane";
import {
  listModels,
  sendChat,
  resolveEdit,
  resolveCode,
  listThreads,
  checkDirectoriesExist,
  createThread,
  loadThread,
  listTrashed,
  currentDatetime,
  stopTurn,
  revealNote,
  revealFolder,
  readProjectFile,
  readNote,
  extractDocument,
  loadThreadConfig,
  saveThreadConfig,
  type Compaction,
  setApiKey,
  hasApiKey,
  deleteApiKey,
  type Message,
  type Segment,
  type StreamEvent,
  type ConnRoute,
  getPermissions,
  setPermissions,
  listActions,
  type ActionInfo,
  warmSearchIndex,
  startRepoIndexing,
  stopRepoIndexing,
  type IndexProgress,
} from "./lib/provider";
import { runPython, preloadPython, cancelPython } from "./lib/python";
import "./App.css";
// KaTeX stylesheet + fonts. `katex` bundles its own web fonts, which Vite emits
// as local assets — so math renders fully offline, no CDN (keeps the no-supply-
// lines promise intact for the packaged app).
import "katex/dist/katex.min.css";
// highlight.js token colours for fenced code. Bundled locally (offline); the
// theme's own `.hljs` background is overridden by our higher-specificity
// `.markdown pre code` rule, so only the syntax colours land on our chrome.
// Swap this one line for another highlight.js theme (e.g. base16/gruvbox-dark-
// medium) to restyle all code blocks.
import "highlight.js/styles/atom-one-dark.css";
// Pure helpers, constants, and shared types extracted from this file — see
// utils.ts for the full list. No React, no JSX.
import {
  CONNECTIONS,
  CODE_CORE,
  CHAT_TOOLS_DEFAULT,
  SESSION_TOOLS_DEFAULT,
  toolTitle,
  loadPrefs,
  savePrefs,
  withEditedText,
  type Connection,
  type Prefs,
  type ThreadTools,
} from "./utils";

async function readFileAsText(file: File): Promise<string> {
  const lower = file.name.toLowerCase();
  if (lower.endsWith(".docx") || lower.endsWith(".pdf")) {
    const bytes = Array.from(new Uint8Array(await file.arrayBuffer()));
    return await extractDocument(file.name, bytes);
  }
  return await file.text();
}

function App() {
  const prefsRef = useRef<Prefs | null>(null);
  if (prefsRef.current === null) prefsRef.current = loadPrefs();
  const prefs0 = prefsRef.current;

  const [theme, setThemeState] = useState<"dark" | "light">(
    () => prefs0.theme ?? "dark",
  );

  const setTheme = (nextTheme: "dark" | "light") => {
    setThemeState(nextTheme);
    savePrefs({ theme: nextTheme });
    document.documentElement.setAttribute("data-theme", nextTheme);
  };

  useEffect(() => {
    document.documentElement.setAttribute("data-theme", theme);
  }, [theme]);

  const {
    models,
    setModels,
    model,
    setModel,
    keyStatus,
    setKeyStatus,
    samplingOn,
    setSamplingOn,
    temperature,
    setTemperature,
    topP,
    setTopP,
    maxTokens,
    setMaxTokens,
    thinkingEffort,
    setThinkingEffort,
    maxContext,
    setMaxContext,
    maxToolCalls,
    setMaxToolCalls,
    mdWriteOn,
    setMdWriteOn,
    notesOn,
    setNotesOn,
    htmlWriteOn,
    setHtmlWriteOn,
    codeOn,
    setCodeOn,
  } = useModelConfig(prefs0);

  const [now, setNow] = useState("");
  const [menuUp, setMenuUp] = useState(false);
  const [showTrash, setShowTrash] = useState(false);
  const [added, setAdded] = useState<string[]>([]);
  const [madeInChat, setMadeInChat] = useState<string[]>([]);
  const [stagedFiles, setStagedFiles] = useState<string[]>([]);

  function stageFile(path: string) {
    setStagedFiles((prev) => (prev.includes(path) ? prev : [...prev, path]));
  }

  function unstageFile(path: string) {
    setStagedFiles((prev) => prev.filter((p) => p !== path));
  }

  function toggleStagedFile(path: string) {
    setStagedFiles((prev) =>
      prev.includes(path) ? prev.filter((p) => p !== path) : [...prev, path],
    );
  }

  function isStaged(path: string): boolean {
    return stagedFiles.includes(path);
  }

  const [stagedExternalFiles, setStagedExternalFiles] = useState<ExternalFileAttachment[]>([]);

  function stageExternalFile(file: ExternalFileAttachment) {
    setStagedExternalFiles((prev) =>
      prev.some((f) => f.name === file.name)
        ? prev.map((f) => (f.name === file.name ? file : f))
        : [...prev, file],
    );
  }

  function unstageExternalFile(name: string) {
    setStagedExternalFiles((prev) => prev.filter((f) => f.name !== name));
  }

  // Record a new document's provenance (uploaded vs model-made). Uploaded files
  // ("added") are staged for the next turn; model-created files ("chat") are listed under
  // "Made in chat" in the drawer without auto-staging.
  function trackDocument(path: string, source: "chat" | "added") {
    if (source === "added") {
      setAdded((a) => (a.includes(path) ? a : [...a, path]));
      stageFile(path);
    } else {
      setMadeInChat((m) => (m.includes(path) ? m : [...m, path]));
    }
  }
  const [systemPrompt, setSystemPrompt] = useState("");
  const [systemScope, setSystemScope] = useState<"global" | "local">("global");
  // Side-split globals (2026-08-24): one global system prompt per surface.
  // Legacy single `system_global` seeds both so old prefs keep working.
  const [globalSystemChat, setGlobalSystemChat] = useState(
    prefs0.system_global_chat ?? prefs0.system_global ?? "",
  );
  const [globalSystemCode, setGlobalSystemCode] = useState(
    prefs0.system_global_code ?? prefs0.system_global ?? "",
  );
  const [showSystem, setShowSystem] = useState(false);
  const [systemDraft, setSystemDraft] = useState("");
  const [draftScope, setDraftScope] = useState<
    "global_chat" | "global_code" | "local"
  >("global_chat");
  const [profileName, setProfileName] = useState(prefs0.profile_name ?? "");
  const [profilePreferred, setProfilePreferred] = useState(
    prefs0.profile_preferred ?? "",
  );
  const [profileAbout, setProfileAbout] = useState(prefs0.profile_about ?? "");
  const [profileShare, setProfileShare] = useState(prefs0.profile_share ?? false);
  // Off by default: gates the in-place edit affordance on assistant messages.
  const [editMessages, setEditMessages] = useState(prefs0.edit_messages ?? false);
  // Connections: which model source is active, per-source base-URL overrides,
  // and which sources have a key stored (the keychain never hands back the key).
  const [activeConn, setActiveConn] = useState(prefs0.active_conn ?? "lmstudio");
  const [connBaseUrls, setConnBaseUrls] = useState<Record<string, string>>(
    prefs0.conn_base_urls ?? {},
  );
  const [showSettings, setShowSettings] = useState(false);
  const [availableUpdate, setAvailableUpdate] = useState<Update | null>(null);
  // Which page of the settings modal is showing. Settings is a modal overlay
  // (opened from the sidebar user button); the nav switches sections.
  const [settingsSection, setSettingsSection] = useState<
    "general" | "connections" | "coding" | "profile" | "updates" | "guide"
  >("general");
  // Selective auto-approval policy (backend-persisted). Default fully gated.
  const [permAutoWrites, setPermAutoWrites] = useState(false);
  const [permAutoCommands, setPermAutoCommands] = useState(false);
  const [permWhitelist, setPermWhitelist] = useState<string[]>([]);
  const [permDraft, setPermDraft] = useState("");
  const [view, setView] = useState<"chat" | "code">("chat");
  // The active coding session's project folder (null for ordinary chat threads);
  // its presence turns on the project-file tools.
  const [sessionRoot, setSessionRoot] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  // A single themed, centred in-app modal for every dialog except the native
  // folder picker — replaces window.confirm/prompt (which the webview pins to the
  // top) and the OS `ask` dialog (which clashes with the dark theme). A "prompt"
  // resolves to the entered string (or null); a "confirm" resolves to a boolean.
  const [modal, setModal] = useState<
    | {
        kind: "prompt";
        title: string;
        value: string;
        resolve: (v: string | null) => void;
      }
    | {
        kind: "confirm";
        title: string;
        message: string;
        resolve: (v: boolean) => void;
      }
    | null
  >(null);
  const [keyDraft, setKeyDraft] = useState<Record<string, string>>({});
  const hydratedThread = useRef<number | null>(null);
  // Monotonic ticket for connection switches, so a slow model-list reply from a
  // source the user has already moved on from can't overwrite the current one.
  const connSwitchSeq = useRef(0);
  // One-shot: on the first thread load (startup), snap the view to match the
  // thread's kind, so a restored session opens in Code (not Chat).
  const didInitView = useRef(false);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const [showToggles, setShowToggles] = useState(false);
  const [toolsOn, setToolsOn] = useState(prefs0.toolsOn ?? true);
  const [webSearchOn, setWebSearchOn] = useState(prefs0.webSearchOn ?? false);
  const [webFetchOn, setWebFetchOn] = useState(prefs0.webFetchOn ?? true);
  const [webOn, setWebOn] = useState(prefs0.webOn ?? false);
  const [visionOn, setVisionOn] = useState(prefs0.visionOn ?? false);
  const [godotToolsOn, setGodotToolsOn] = useState(prefs0.godotToolsOn ?? false);
  const [browserOn, setBrowserOn] = useState(prefs0.browserOn ?? false);
  const [liteTools, setLiteTools] = useState(prefs0.liteTools ?? false);
  const [codeGroupsOn, setCodeGroupsOn] = useState<Record<string, boolean>>(
    () => prefs0.codeGroups ?? {},
  );
  const [readPruneTurns, setReadPruneTurns] = useState<number | null>(
    () => prefs0.read_prune_turns ?? 2,
  );
  const {
    messages,
    setMessages,
    segments,
    setSegments,
    pendingTool,
    setPendingTool,
    pendingEdit,
    setPendingEdit,
    busy,
    setBusy,
    isStopping,
    setIsStopping,
    error,
    setError,
    editingIdx,
    setEditingIdx,
    editDraft,
    setEditDraft,
    copiedIdx,
    images,
    setImages,
    addImageFiles,
    copyMessage,
    startEdit,
    cancelEdit,
  } = useTurnRunner();

  const addExternalFiles = useCallback(
    async (files: FileList | File[]) => {
      const list = Array.from(files);
      for (const f of list) {
        if (f.type.startsWith("image/")) {
          await addImageFiles([f]);
          continue;
        }
        try {
          if (f.size > 2 * 1024 * 1024) {
            setError(`File "${f.name}" exceeds the 2MB size limit for text attachments.`);
            continue;
          }
          const content = await readFileAsText(f);
          if (content.includes("\0")) {
            setError(`Cannot attach binary file "${f.name}".`);
            continue;
          }
          stageExternalFile({
            name: f.name,
            path: (f as any).path || undefined,
            content,
          });
        } catch (err) {
          setError(`Failed to read "${f.name}": ${err}`);
        }
      }
    },
    [addImageFiles, setError],
  );

  const scrollRef = useRef<HTMLDivElement>(null);
  const transcriptInnerRef = useRef<HTMLDivElement>(null);
  const stickRef = useRef(true);
  // Set only around our own auto-scroll, so the scroll event it fires isn't mistaken
  // for the reader scrolling away (which would detach us from the bottom).
  const autoScrollRef = useRef(false);
  // How close to the bottom still counts as "at the bottom". Non-zero only to absorb
  // sub-pixel scrollTop rounding — a wider slack is what makes detaching feel grabby,
  // so it stays tiny; robustness comes from the echo guard + ResizeObserver instead.
  const BOTTOM_EPSILON = 2;

  const [actions, setActions] = useState<ActionInfo[]>([]);
  const refreshActions = async () => {
    try {
      const acts = await listActions(sessionRoot);
      setActions(acts);
    } catch (e) {
      console.error("Failed to load actions:", e);
    }
  };

  useEffect(() => {
    void refreshActions();
  }, [sessionRoot]);

  // The active connection's definition, and the route (kind + base URL + key id)
  // the backend uses to build the right provider for a turn / model list.
  function activeConnDef(): Connection {
    return CONNECTIONS.find((c) => c.id === activeConn) ?? CONNECTIONS[0];
  }

  // Themed confirm dialog. Returns true if the user confirms.
  function confirmBox(message: string, title: string): Promise<boolean> {
    return new Promise((resolve) =>
      setModal({ kind: "confirm", title, message, resolve }),
    );
  }

  // Themed text prompt. Returns the entered string, or null if cancelled.
  function promptBox(title: string, initial: string): Promise<string | null> {
    return new Promise((resolve) =>
      setModal({ kind: "prompt", title, value: initial, resolve }),
    );
  }

  function clearTurnState() {
    stickRef.current = true;
    setSegments([]);
    setPendingEdit(null);
    setPendingTool(null);
    setEditingIdx(null);
  }

  const {
    threads,
    setThreads,
    trashed,
    setTrashed,
    currentThreadId,
    selectThreadId,
    menuThreadId,
    setMenuThreadId,
    exportDoc,
    exportName,
    setExportName,
    exportSaved,
    exportRaw,
    setExportRaw,
    threadIdRef,
    persist,
    switchThread,
    newChat,
    newSession,
    changeThreadFolder,
    switchView,
    doDuplicate,
    removeThread,
    restoreFromTrash,
    purgeFromTrash,
    emptyTrash,
    renameThreadById,
    openExportThreadById,
    openExportMessage,
    closeExport,
    doExport,
    setOptions: setThreadOptions,
  } = useThreadManager();

  // The active connection route (kind + base URL + key id)
  // the backend uses to build the right provider for a turn / model list.
  function connRoute(): ConnRoute {
    const c = activeConnDef();
    return {
      kind: c.kind,
      baseUrl: connBaseUrls[c.id] ?? c.baseUrl,
      keyId: c.id,
      midSystemMessages: c.midSystemMessages !== false,
    };
  }

  // Sampling overrides (spec §5 QoL). Off by default — when off, nothing is sent
  // and the model keeps its own defaults; when on, the slider values are applied.
  function samplingParams() {
    if (!samplingOn) return {};
    const mt = parseInt(maxTokens, 10);
    return {
      temperature,
      top_p: topP,
      max_tokens: Number.isFinite(mt) && mt > 0 ? mt : null,
    };
  }

  // Max context (tokens) to send — a pricing-tier guard. Empty / 0 / non-numeric
  // means no cap (use the model's full window). Applies to every provider.
  function contextCap(): number | null {
    const n = parseInt(maxContext, 10);
    return Number.isFinite(n) && n > 0 ? n : null;
  }

  /** Null = let the backend use its default. Deliberately NOT clamped here: the
   *  backend clamps anyway, and doing it in both places means two ceilings to
   *  keep in step. This only decides "did the user type a usable number". */
  function toolCallCap(): number | null {
    const n = parseInt(maxToolCalls, 10);
    return Number.isFinite(n) && n > 0 ? n : null;
  }

  /** The system prompt in effect: the thread-local one when scoped local,
   *  otherwise the side-appropriate global (chat or code). */
  function activeSystem(): string {
    if (systemScope === "local") return systemPrompt;
    return view === "code" ? globalSystemCode : globalSystemChat;
  }

  function systemForModel(): string {
    return activeSystem();
  }

  // The user's profile as a natural-language note for the model. Phrased as
  // reference context (not a directive) and placed last in the system slot by
  // the backend, so the model treats it as information it has rather than as
  // instructions baked into its identity.
  function profileText(): string {
    const name = profileName.trim();
    const pref = profilePreferred.trim();
    const about = profileAbout.trim();
    if (!name && !pref && !about) return "";
    const lines: string[] = [
      "For context, a little about the person you're talking with — " +
        "background to draw on naturally where it's relevant, not instructions to act on.",
    ];
    if (pref && name) lines.push(`They go by ${pref} (full name ${name}).`);
    else if (pref) lines.push(`They go by ${pref}.`);
    else if (name) lines.push(`Their name is ${name}.`);
    if (about) lines.push(`In their words: ${about}`);
    return lines.join("\n\n");
  }

  // The profile text actually sent (empty unless the user has opted to share).
  function profileForModel(): string {
    return profileShare ? profileText() : "";
  }

  function enabledToolList(): string[] {
    let t: string[] = [];
    // A coding session is its OWN surface with its own curated set — not the chat
    // tools plus project tools. Project tools are always on (they're the
    // point); web research still makes sense, honoring its toggles. The notes /
    // markdown / html authoring tools target the notes folder, not the project, so
    // they're chat-only — useless-to-harmful in a session, and excluded here.
    if (sessionRoot) {
      // Core is always present: orientation, reads, declarative single-file edits
      // & writes, safe file management (delete/rename/restore), commands,
      // and background dev servers (CODE_CORE).
      t.push(...CODE_CORE);
      // Elective capabilities:
      if (webOn || (toolsOn && (webSearchOn || webFetchOn))) {
        t.push("web_search", "web_fetch");
      }
      if (godotToolsOn) t.push("godot_scene_tree");
      if (browserOn) {
        t.push(
          "browser_open",
          "browser_read",
          "browser_act",
          "browser_reload",
          "browser_close",
        );
      }
      if (visionOn) {
        t.push("read_image");
        if (browserOn) t.push("browser_screenshot");
      }
      return t;
    }
    // Chat surface: assistant / notes tools, behind the general Tools toggle.
    if (!toolsOn) return t;
    if (webSearchOn) t.push("web_search");
    if (webFetchOn) t.push("web_fetch");
    // Markdown notes: write, edit, search, and read — bundled as one coherent
    // library suite (matching the html-write bundle).
    if (mdWriteOn || notesOn) {
      t.push("md_write");
      t.push("md_edit");
      t.push("search_notes");
      t.push("read_note");
    }
    // HTML is one bundle by design: rarely wanted, heavy when it is, so it
    // arrives and leaves as a unit rather than as three switches to manage.
    if (htmlWriteOn) {
      t.push("html_write");
      t.push("html_edit");
      t.push("search_pages");
      if (!t.includes("read_note")) t.push("read_note");
    }
    if (codeOn) {
      t.push("run_python");
    }
    return t;
  }

  const {
    compaction,
    setCompaction,
    preview,
    showInspector,
    setShowInspector,
    nudgeDismissed,
    setNudgeDismissed,
    forModel,
    runCompaction,
    contextFileList,
    contextPressure,
    isCompacting,
  } = useCompactionManager({
    messages,
    currentThreadId,
    threadIdRef,
    sessionRoot,
    busy,
    model,
    connRoute,
    contextCap,
    setBusy,
    setError,
    setNotice,
    enabledToolList,
    systemForModel,
    profileForModel,
    visionOn,
    toolsOn,
    webSearchOn,
    webFetchOn,
    webOn,
    browserOn,
    codeGroupsOn,
    liteTools,
    godotToolsOn,
    mdWriteOn,
    notesOn,
    htmlWriteOn,
    codeOn,
    activeConn,
    connBaseUrls,
    systemPrompt,
    globalSystem: view === "code" ? globalSystemCode : globalSystemChat,
    systemScope,
    profileShare,
    profileName,
    profilePreferred,
    profileAbout,
    maxContext,
    readPruneTurns,
    stagedFiles,
    stagedExternalFiles,
  });

  const {
    filesOpen,
    setFilesOpen,
    filesWidth,
    notes,
    pages,
    viewingNote,
    noteContent,
    noteEditing,
    setNoteEditing,
    noteDraft,
    setNoteDraft,
    noteRaw,
    setNoteRaw,
    compactionMeta,
    viewerKind,
    projectDocs,
    projectFiles,
    expandedDirs,
    toggleDir,
    modifiedFiles,
    showDiff,
    setShowDiff,
    fileDiff,
    loadFileDiff,
    folderMissing,
    refreshModifiedFiles,
    openProjectFile,
    refreshProjectFiles,
    refreshNotes,
    openPageInBrowser,
    openPageSource,
    openFiles,
    openNote,
    openProjectDoc,
    refreshProjectDocs,
    openCompaction,
    closeNote,
    startNoteEdit,
    saveNote,
    exportNoteDocx,
    deleteNoteByName,
    renameNoteFromViewer,
    startFilesResize,
    handleUpload,
  } = useFileManager({
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
  });

  const [missingRoots, setMissingRoots] = useState<Set<string>>(new Set());

  const [indexingStatus, setIndexingStatus] = useState<{
    active: boolean;
    current: number;
    total: number;
    done: boolean;
    message?: string;
  } | null>(null);

  // Auto-dismiss completed or stopped indexing status after 7 seconds
  useEffect(() => {
    if (!indexingStatus || indexingStatus.active) return;
    const timer = setTimeout(() => {
      setIndexingStatus(null);
    }, 7000);
    return () => clearTimeout(timer);
  }, [indexingStatus]);

  function startIndexing() {
    if (!sessionRoot) return;
    setIndexingStatus({
      active: true,
      current: 0,
      total: 0,
      done: false,
      message: "preparing search index…",
    });

    startRepoIndexing(sessionRoot, (p: IndexProgress) => {
      if (p.status === "up_to_date" || p.status === "done") {
        setIndexingStatus({
          active: false,
          current: p.total,
          total: p.total,
          done: true,
          message:
            p.message ||
            `✓ Vector cache complete — ${p.total.toLocaleString()} chunks ready.`,
        });
      } else if (p.status === "cancelled") {
        setIndexingStatus({
          active: false,
          current: p.current,
          total: p.total,
          done: true,
          message:
            p.message ||
            `Vector indexing stopped (${p.current.toLocaleString()} / ${p.total.toLocaleString()} chunks cached).`,
        });
      } else if (p.status === "error") {
        setIndexingStatus(null);
        setError(p.message || "Failed to index repository.");
      } else {
        // "indexing"
        const pct = p.total > 0 ? Math.round((p.current / p.total) * 100) : 0;
        setIndexingStatus({
          active: true,
          current: p.current,
          total: p.total,
          done: false,
          message: `indexing codebase (${p.current.toLocaleString()} / ${p.total.toLocaleString()} chunks · ${pct}%)…`,
        });
      }
    }).catch((err) => {
      setIndexingStatus(null);
      setError(`Indexing failed: ${err}`);
    });
  }

  function handleStopIndexing() {
    stopRepoIndexing().catch(() => {});
  }

  // Check which session project folders exist on disk
  useEffect(() => {
    const roots = Array.from(
      new Set(
        threads
          .map((t) => t.project_root)
          .filter((r): r is string => Boolean(r && r.trim())),
      ),
    );
    if (roots.length === 0) {
      setMissingRoots(new Set());
      return;
    }
    let cancelled = false;
    checkDirectoriesExist(roots)
      .then((results) => {
        if (cancelled) return;
        const missing = new Set<string>();
        results.forEach((exists, idx) => {
          if (!exists) missing.add(roots[idx]);
        });
        setMissingRoots(missing);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [threads]);

  // Also verify on window focus (user may have moved/restored a folder in Explorer)
  useEffect(() => {
    const onFocus = () => {
      const roots = Array.from(
        new Set(
          threads
            .map((t) => t.project_root)
            .filter((r): r is string => Boolean(r && r.trim())),
        ),
      );
      if (roots.length === 0) return;
      checkDirectoriesExist(roots)
        .then((results) => {
          const missing = new Set<string>();
          results.forEach((exists, idx) => {
            if (!exists) missing.add(roots[idx]);
          });
          setMissingRoots(missing);
        })
        .catch(() => {});
    };
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [threads]);

  // Suppress browser navigation context menu (Back, Forward, Reload, Save as) across general UI chrome,
  // but preserve native context menu when text is selected (to Copy snippets with the mouse) or inside
  // editable fields (to Cut/Copy/Paste text) and links.
  useEffect(() => {
    const handleContextMenu = (e: MouseEvent) => {
      // 1. If text is highlighted/selected anywhere, allow the menu so the user can Copy with the mouse.
      const selection = window.getSelection();
      if (selection && !selection.isCollapsed && selection.toString().trim().length > 0) {
        return;
      }

      const target = e.target as HTMLElement | null;
      if (!target) {
        e.preventDefault();
        return;
      }

      // 2. Allow inside editable inputs/textareas for Cut/Copy/Paste.
      if (
        target.tagName === "INPUT" ||
        target.tagName === "TEXTAREA" ||
        target.isContentEditable ||
        target.closest("input, textarea, [contenteditable='true']")
      ) {
        return;
      }

      // 3. Allow on links (for Copy link address) and images.
      if (target.tagName === "A" || target.closest("a") || target.tagName === "IMG") {
        return;
      }

      // 4. Otherwise prevent default browser webview menu.
      e.preventDefault();
    };
    window.addEventListener("contextmenu", handleContextMenu);
    return () => window.removeEventListener("contextmenu", handleContextMenu);
  }, []);

  const isFolderMissing = Boolean(
    sessionRoot && (folderMissing || missingRoots.has(sessionRoot)),
  );

  setThreadOptions({
    busy,
    setError,
    clearTurnState,
    setMessages,
    sessionRoot,
    setSessionRoot,
    view,
    setView,
    confirmBox,
    promptBox,
    closeNote,
    refreshNotes,
  });

  const handleChangeSessionRoot = useCallback(() => {
    if (currentThreadId != null) {
      void changeThreadFolder(currentThreadId);
    }
  }, [changeThreadFolder, currentThreadId]);

  const prevSessionRootRef = useRef<string | null>(sessionRoot);
  useEffect(() => {
    if (prevSessionRootRef.current !== sessionRoot) {
      prevSessionRootRef.current = sessionRoot;
      setStagedFiles([]);
      setStagedExternalFiles([]);
      if (sessionRoot) {
        warmSearchIndex(sessionRoot).catch(() => {});
      }
    }
  }, [sessionRoot]);

  useEffect(() => {
    listModels(connRoute())
      .then((m) => {
        setModels(m);
        const saved = prefs0.model ?? "";
        if (m.length > 0 && !m.includes(saved)) setModel(m[0]);
      })
      .catch((e) =>
        setError(
          `Couldn't reach ${activeConnDef().label} — is the source running/reachable, and its key set? (${e})`,
        ),
      );
    // Load which connections already have a key stored (the menu shows "key set ✓").
    CONNECTIONS.filter((c) => c.needsKey).forEach((c) => {
      hasApiKey(c.id)
        .then((set) => setKeyStatus((s) => ({ ...s, [c.id]: set })))
        .catch(() => {});
    });
    // The web-search (Tavily) key is stored the same way, under id "tavily".
    hasApiKey("tavily")
      .then((set) => setKeyStatus((s) => ({ ...s, tavily: set })))
      .catch(() => {});
  }, []);


  // Live current-time readout — the same date the model is given (§10e).
  useEffect(() => {
    const tick = () => {
      currentDatetime()
        .then(setNow)
        .catch(() => {});
    };
    tick();
    const handle = setInterval(tick, 60_000);
    return () => clearInterval(handle);
  }, []);

  // Load thread history on startup; create an empty thread if there are none.
  useEffect(() => {
    (async () => {
      try {
        let ts = await listThreads();
        if (ts.length === 0) {
          const id = await createThread();
          selectThreadId(id);
          setMessages([]);
          ts = await listThreads();
        } else {
          selectThreadId(ts[0].id);
          setMessages(await loadThread(ts[0].id));
        }
        setThreads(ts);
        setTrashed(await listTrashed());
      } catch (e) {
        setError(`Couldn't load history: ${e}`);
      }
    })();
  }, []);

  // Check for updates silently on startup.
  useEffect(() => {
    let active = true;
    check()
      .then((u) => {
        if (active && u) {
          setAvailableUpdate(u);
        }
      })
      .catch(() => {
        // Silently ignore if offline, in development mode, or endpoint unreachable.
      });
    return () => {
      active = false;
    };
  }, []);

  // Keep the transcript pinned while the reader is at the bottom. Fires on content
  // change; the ResizeObserver below covers the reflow that *follows* it.
  useEffect(() => {
    if (stickRef.current) pinToBottom();
  }, [messages, segments, pendingTool, pendingEdit]);

  // Late layout — an approval card's diff, a code block growing, a font landing —
  // changes the content height with no React state change, which used to strand the
  // view a little above the bottom while it still looked "at" the bottom. Re-pin when
  // the content resizes, if the reader is sticky.
  useEffect(() => {
    const content = transcriptInnerRef.current;
    if (!content || typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(() => {
      if (stickRef.current) pinToBottom();
    });
    ro.observe(content);
    return () => ro.disconnect();
  }, []);

  // Apply a thread's tool switches (the `tools` blob from its config), falling
  // back to KIND defaults for a thread with none — a new thread, or a
  // pre-`tools` thread (migration is "absent → defaults", no snapshot — user
  // decision 2026-08-24). The kind comes from `project_root`: null = chat,
  // set = coding session.
  function applyThreadTools(t: ThreadTools) {
    setToolsOn(t.toolsOn);
    setWebSearchOn(t.webSearchOn);
    setWebFetchOn(t.webFetchOn);
    const resolvedWebOn = t.webOn ?? (t.webSearchOn || t.webFetchOn) ?? false;
    setWebOn(resolvedWebOn);
    setVisionOn(t.visionOn);
    setGodotToolsOn(t.godotToolsOn);
    const resolvedBrowserOn =
      t.browserOn ??
      (t.liteTools ? !!t.codeGroupsOn?.browser : false);
    setBrowserOn(resolvedBrowserOn);
    setLiteTools(t.liteTools ?? false);
    setCodeGroupsOn(t.codeGroupsOn ?? {});
    setMdWriteOn(t.mdWriteOn);
    setNotesOn(t.notesOn);
    setHtmlWriteOn(t.htmlWriteOn);
    setCodeOn(t.codeOn);
    setSamplingOn(t.samplingOn);
    setTemperature(t.temperature);
    setTopP(t.topP);
    setMaxTokens(t.maxTokens);
  }

  // Load the per-chat config (documents + system prompt + tool switches) when
  // the thread changes.
  useEffect(() => {
    if (currentThreadId == null) {
      hydratedThread.current = null;
      setAdded([]);
      setMadeInChat([]);
      setStagedFiles([]);
      setStagedExternalFiles([]);
      setCompaction(null);
      setSystemPrompt("");
      setSystemScope("global");
      applyThreadTools(
        view === "code" ? SESSION_TOOLS_DEFAULT : CHAT_TOOLS_DEFAULT,
      );
      return;
    }
    // If runTurn just created, configured, and marked this thread hydrated,
    // skip the redundant loadThreadConfig re-fetch to avoid clobbering live turn state.
    if (hydratedThread.current === currentThreadId) {
      return;
    }
    let cancelled = false;
    hydratedThread.current = null;
    const firstLoad = !didInitView.current;
    didInitView.current = true;
    loadThreadConfig(currentThreadId)
      .then((raw) => {
        if (cancelled) return;
        try {
          const cfg = JSON.parse(raw) as {
            added?: string[];
            made_in_chat?: string[];
            context?: string[];
            context_files?: { path: string; on: boolean }[]; // legacy shape
            system_prompt?: string;
            system_scope?: "global" | "local";
            project_root?: string;
            compaction?: Compaction;
            tools?: Partial<ThreadTools>;
          };
          setAdded(cfg.added ?? []);
          setMadeInChat(cfg.made_in_chat ?? []);
          setStagedFiles([]);
          setStagedExternalFiles([]);
          setCompaction(cfg.compaction ?? null);
          // Per-thread tool switches: the thread's own set if it has one, else
          // kind defaults.
          applyThreadTools({
            ...(cfg.project_root ? SESSION_TOOLS_DEFAULT : CHAT_TOOLS_DEFAULT),
            ...(cfg.tools ?? {}),
          } as ThreadTools);
          setSystemPrompt(cfg.system_prompt ?? "");
          setSystemScope(
            cfg.system_scope ?? (cfg.system_prompt ? "local" : "global"),
          );
          setSessionRoot(cfg.project_root ?? null);
          // Startup only: if the restored thread is a session, land in Code view.
          if (firstLoad && cfg.project_root) setView("code");
        } catch {
          setAdded([]);
          setMadeInChat([]);
          setStagedFiles([]);
          setStagedExternalFiles([]);
          setCompaction(null);
          applyThreadTools(CHAT_TOOLS_DEFAULT);
          setSystemPrompt("");
          setSystemScope("global");
          setSessionRoot(null);
        }
        hydratedThread.current = currentThreadId;
      })
      .catch(() => {
        if (cancelled) return;
        setAdded([]);
        setMadeInChat([]);
        setStagedFiles([]);
        setStagedExternalFiles([]);
        setCompaction(null);
        applyThreadTools(CHAT_TOOLS_DEFAULT);
        setSystemPrompt("");
        setSystemScope("global");
        setSessionRoot(null);
        hydratedThread.current = currentThreadId;
      });
    return () => {
      cancelled = true;
    };
  }, [currentThreadId, view]);

  // Persist the per-chat config when documents, the system prompt, or the
  // per-thread TOOL SWITCHES change — but only once this thread's config has
  // hydrated, so navigating between threads never writes one thread's state
  // onto another.
  useEffect(() => {
    if (currentThreadId == null) return;
    if (hydratedThread.current !== currentThreadId) return;
    saveThreadConfig(
      currentThreadId,
      JSON.stringify({
        added,
        made_in_chat: madeInChat,
        system_prompt: systemPrompt,
        system_scope: systemScope,
        project_root: sessionRoot ?? undefined,
        compaction: compaction ?? undefined,
        tools: {
          toolsOn,
          webSearchOn,
          webFetchOn,
          webOn,
          visionOn,
          godotToolsOn,
          browserOn,
          liteTools,
          codeGroupsOn,
          mdWriteOn,
          notesOn,
          htmlWriteOn,
          codeOn,
          samplingOn,
          temperature,
          topP,
          maxTokens,
        },
      }),
    ).catch(() => {});
  }, [added, madeInChat, systemPrompt, systemScope, sessionRoot, compaction, currentThreadId, toolsOn, webSearchOn, webFetchOn, webOn, visionOn, godotToolsOn, browserOn, liteTools, codeGroupsOn, mdWriteOn, notesOn, htmlWriteOn, codeOn, samplingOn, temperature, topP, maxTokens]);

  // Persist global UI preferences (theme, model + tools) across launches. Sampling is
  // per-thread now (ThreadTools), so it is deliberately NOT persisted here.
  useEffect(() => {
    const prefs: Prefs = {
      theme,
      model,
      toolsOn,
      webSearchOn,
      webFetchOn,
      webOn,
      visionOn,
      godotToolsOn,
      browserOn,
      mdWriteOn,
      notesOn,
      htmlWriteOn,
      codeOn,
      thinkingEffort,
      maxContext,
      maxToolCalls,
      system_global_chat: globalSystemChat,
      system_global_code: globalSystemCode,
      profile_name: profileName,
      profile_preferred: profilePreferred,
      profile_about: profileAbout,
      profile_share: profileShare,
      active_conn: activeConn,
      conn_base_urls: connBaseUrls,
      edit_messages: editMessages,
      liteTools,
      codeGroups: codeGroupsOn,
      read_prune_turns: readPruneTurns,
    };
    savePrefs(prefs);
  }, [
    theme,
    model,
    toolsOn,
    webSearchOn,
    webFetchOn,
    webOn,
    visionOn,
    godotToolsOn,
    browserOn,
    mdWriteOn,
    notesOn,
    htmlWriteOn,
    codeOn,
    thinkingEffort,
    maxContext,
    maxToolCalls,
    globalSystemChat,
    globalSystemCode,
    profileName,
    profilePreferred,
    profileAbout,
    profileShare,
    activeConn,
    connBaseUrls,
    editMessages,
    liteTools,
    codeGroupsOn,
    readPruneTurns,
  ]);

  // Warm up the Python sandbox in the background once enabled, so the first run
  // doesn't pay the one-time Pyodide load.
  useEffect(() => {
    if (codeOn) preloadPython();
  }, [codeOn]);

  async function answerEdit(
    decision: "approve" | "reject" | "rename" | "remember",
    feedback?: string,
  ) {
    if (!pendingEdit) return;
    const path = pendingEdit.path;
    const tool = pendingEdit.tool;
    // "Always allow" = approve, plus persist the whitelist patterns the backend
    // computed for this command (the leading token of every segment, so a compound
    // like `git add -A && git commit` is covered too). Read the policy fresh (not the
    // possibly stale permWhitelist state) so a concurrent settings edit isn't
    // clobbered. A failed persist still approves — running the command is the point —
    // but is surfaced so the remember isn't a silent no-op.
    if (decision === "remember" && pendingEdit.command) {
      const patterns = pendingEdit.command.remember;
      try {
        const p = await getPermissions();
        const list = [...p.command_whitelist];
        for (const pattern of patterns) {
          if (!list.includes(pattern)) list.push(pattern);
        }
        if (list.length !== p.command_whitelist.length || !p.auto_commands) {
          await setPermissions(p.auto_project_writes, true, list);
          setPermWhitelist(list);
          setPermAutoWrites(p.auto_project_writes);
          setPermAutoCommands(true);
        }
        const names = patterns.map((x) => `“${x}”`).join(", ");
        setNotice(`Always allow ${names} — manage in Settings → Permissions & Safety.`);
      } catch (e) {
        setError(`Couldn't save the auto-approval rule: ${e}`);
      }
      decision = "approve";
    }
    const wireDecision =
      decision === "reject" && feedback?.trim()
        ? `reject:${feedback.trim()}`
        : decision;
    resolveEdit(pendingEdit.id, wireDecision);
    setPendingEdit(null);
    // reject/rename write nothing — the model handles the retry.
    if (decision !== "approve") return;
    if (
      tool === "write_file" ||
      tool === "edit_file" ||
      tool === "delete_file" ||
      tool === "rename_file" ||
      tool === "restore_file" ||
      tool === "project_write" ||
      tool === "project_edit" ||
      tool === "project_delete" ||
      tool === "project_rename" ||
      tool === "project_restore" ||
      tool === "run_background_process" ||
      tool === "process_start" ||
      tool === "run_command"
    ) {
      // Project actions aren't notes — skip the made-in-chat tracking and the note
      // viewer (which resolves against the notes root and 404s a project path).
      return;
    }
    const isHtml = /\.html?$/i.test(path);
    if (tool === "html_write" || tool === "html_edit" || isHtml) {
      // HTML lives in the browser + Pages list, never the markdown viewer.
      // Handled politely via in-chip preview button; keep notes/pages list refreshed.
      refreshNotes();
      return;
    }
    if (tool === "md_edit") {
      // Editing an existing file — keep its provenance and context as-is.
    } else {
      // A fresh write — track it as a made-in-chat document (not auto-pinned to tail).
      trackDocument(path, "chat");
    }
    const tid = threadIdRef.current;
    setTimeout(() => {
      if (threadIdRef.current === tid) {
        openNote(path);
      }
    }, 250);
  }

  // Stick to the bottom while the reader is there — but make that state robust, and
  // let it break the instant they scroll away.
  //
  // Two things used to detach the stream even though the reader had not moved: (1) our
  // OWN scrollTo fires a scroll event, whose echo recomputed the distance from a stale
  // bottom and flipped stickiness off; and (2) a strict "< 1px" test, so one extra line
  // ("a double line on run") landed a hair above the floor and detached. The echo is
  // now ignored, and the slack absorbs sub-pixel rounding.
  function handleScroll() {
    const el = scrollRef.current;
    if (!el) return;
    // Our own pin fires a scroll event too — consume it without treating it as a
    // reader signal, or the echo would detach us from the bottom we just jumped to.
    if (autoScrollRef.current) {
      autoScrollRef.current = false;
      return;
    }
    stickRef.current =
      el.scrollHeight - el.scrollTop - el.clientHeight <= BOTTOM_EPSILON;
  }

  // Pin to the bottom (while sticky). A no-op when already there, so it never fires a
  // scroll event we'd then have to ignore.
  function pinToBottom() {
    const el = scrollRef.current;
    if (!el) return;
    if (el.scrollHeight - el.scrollTop - el.clientHeight <= BOTTOM_EPSILON) return;
    autoScrollRef.current = true;
    el.scrollTop = el.scrollHeight;
  }

  // Load the policy once on mount.
  useEffect(() => {
    getPermissions()
      .then((p) => {
        setPermAutoWrites(p.auto_project_writes);
        setPermAutoCommands(p.auto_commands);
        setPermWhitelist(p.command_whitelist);
      })
      .catch(() => {});
  }, []);

  // Persist a policy change (any toggle or whitelist edit) as it's made.
  async function updatePerms(autoWrites: boolean, autoCmds: boolean, list: string[]) {
    setPermAutoWrites(autoWrites);
    setPermAutoCommands(autoCmds);
    setPermWhitelist(list);
    try {
      await setPermissions(autoWrites, autoCmds, list);
    } catch (e) {
      setError(`Couldn't save settings: ${e}`);
    }
  }


  // Re-fetch the registry + live status after any Settings mutation.

  function addPermPattern() {
    const p = permDraft.trim();
    setPermDraft("");
    if (!p || permWhitelist.includes(p)) return;
    updatePerms(permAutoWrites, permAutoCommands, [...permWhitelist, p]);
  }

  // Re-fetch the active source's model list — handy when LM Studio was started
  // after the app, or right after switching source.
  function refreshModels() {
    setError(null);
    listModels(connRoute())
      .then((m) => {
        setModels(m);
        if (m.length === 0) return;
        // Local's list is live server state: the loaded model rides the top, so a
        // refresh means "follow LM Studio's active model" — always snap to m[0].
        // The old keep-if-listed rule only corrected when the previous model
        // vanished from the list entirely; whenever both were listed the selection
        // stayed stale (the 50/50). A hosted list is a catalog, not state — there
        // the user's pick must survive a refresh, so keep it unless it's gone.
        if (activeConn === "lmstudio") setModel(m[0]);
        else if (!m.includes(model)) setModel(m[0]);
      })
      .catch((e) => setError(`Couldn't reach ${activeConnDef().label} (${e})`));
  }

  // Switch model source — intentional, from Settings. Make it active, pull that
  // source's models, and keep the current model only if it still fits the list.
  function switchConnection(id: string) {
    if (id === activeConn) return;
    const c = CONNECTIONS.find((x) => x.id === id);
    if (!c || !c.ready) return;
    setActiveConn(id);
    setError(null);
    setModels([]);
    // Clear the model NOW, and remember it. It belongs to the source we just
    // left; leaving it in place until the new list arrives — or forever, if the
    // fetch fails — means the next send ships a model id the active provider has
    // never heard of, which surfaces as an opaque error from the wrong place.
    const previous = model;
    setModel("");
    // Only the newest switch may write its result. Flipping A→B→C had B's slower
    // reply land after C's and repopulate the dropdown with the wrong provider's
    // models — the same race the consolidation picker already guards against.
    const seq = ++connSwitchSeq.current;
    const route: ConnRoute = {
      kind: c.kind,
      baseUrl: connBaseUrls[c.id] ?? c.baseUrl,
      keyId: c.id,
      midSystemMessages: c.midSystemMessages !== false,
    };
    listModels(route)
      .then((m) => {
        if (seq !== connSwitchSeq.current) return;
        setModels(m);
        // Keep the previous model only if this source actually offers it.
        setModel(m.includes(previous) ? previous : (m[0] ?? ""));
      })
      .catch((e) => {
        if (seq !== connSwitchSeq.current) return;
        setError(`Couldn't reach ${c.label} (${e})`);
      });
  }

  // Store / clear a connection's API key (keychain, backend-side only).
  function saveKey(id: string) {
    const k = (keyDraft[id] ?? "").trim();
    if (!k) return;
    setApiKey(id, k)
      .then(() => {
        setKeyStatus((s) => ({ ...s, [id]: true }));
        setKeyDraft((d) => ({ ...d, [id]: "" }));
      })
      .catch((e) => setError(`Couldn't store key: ${e}`));
  }
  function clearKey(id: string) {
    deleteApiKey(id)
      .then(() => setKeyStatus((s) => ({ ...s, [id]: false })))
      .catch((e) => setError(`Couldn't clear key: ${e}`));
  }



  // The shared turn event handler. The turn-local timeline objects
  // (acc/racc/segs/appendText/toolSegIdx/commit) are carried in `state`.
  const makeHandler = (state: {
    acc: () => string;
    setAcc: (s: string) => void;
    racc: () => string;
    setRacc: (s: string) => void;
    segs: Segment[];
    appendText: (kind: "text" | "reasoning", text: string) => void;
    toolSegIdx: Record<string, number>;
    commit: () => void;
    onDone?: (finishReason: string | null) => void;
  }) => {
    let toolExecutedSinceDelta = false;
    return async (event: StreamEvent) => {
      if (event.type === "delta") {
        const cur = state.acc();
        const needsSep =
          toolExecutedSinceDelta &&
          cur.length > 0 &&
          !cur.endsWith("\n") &&
          !event.text.startsWith("\n");
        const toAdd = needsSep ? "\n\n" + event.text : event.text;
        state.setAcc(cur + toAdd);
        state.appendText("text", toAdd);
        toolExecutedSinceDelta = false;
      } else if (event.type === "reasoning") {
        state.setRacc(state.racc() + event.text);
        state.appendText("reasoning", event.text);
      } else if (event.type === "promote_to_reasoning") {
        // Chat template prefilled `<think>` in the prompt and emitted a closing
        // `</think>` without an opening tag. Promote only the current step's
        // trailing text segment into reasoning so prior iterations' text in a
        // multi-step tool turn stays intact.
        const last = state.segs[state.segs.length - 1];
        if (last && last.kind === "text") {
          const curAcc = state.acc();
          if (curAcc.endsWith(last.text)) {
            state.setAcc(curAcc.slice(0, curAcc.length - last.text.length));
          }
          const promoted = last.text.startsWith("\n\n")
            ? last.text.slice(2)
            : last.text;
          state.setRacc(state.racc() + promoted);
          const prev = state.segs[state.segs.length - 2];
          if (prev && prev.kind === "reasoning") {
            prev.text += promoted;
            state.segs.pop();
          } else {
            (last as unknown as { kind: "reasoning"; text: string }).kind = "reasoning";
            last.text = promoted;
          }
          toolExecutedSinceDelta = state.segs.some((s) => s.kind === "tool");
          state.commit();
        }
      } else if (event.type === "tool_start") {
        state.commit();
        setPendingTool(event.name);
        toolExecutedSinceDelta = true;
      } else if (event.type === "tool_call") {
        setPendingTool(null);
        toolExecutedSinceDelta = true;
        const seg: Segment = {
          kind: "tool",
          id: event.id,
          name: event.name,
          args: event.arguments,
          title: toolTitle(event.name, event.arguments),
          extra_content: event.extra_content,
        };
        if (event.name === "run_python") {
          try {
            seg.code = JSON.parse(event.arguments)?.code ?? "";
          } catch {
            /* partial args — code fills in if/when it parses */
          }
        }
        state.toolSegIdx[event.id] = state.segs.length;
        state.segs.push(seg);
        state.commit();
      } else if (event.type === "tool_result") {
        const idx = state.toolSegIdx[event.id];
        const seg = idx !== undefined ? state.segs[idx] : undefined;
        if (seg && seg.kind === "tool") {
          // Preserve the full tool result verbatim so subsequent turns reconstruct
          // an exact byte-identical Role::Tool message for 100% prompt cache stability,
          // while giving the user full visibility into search and read outputs.
          seg.output = event.result;
          state.commit();
          if (
            seg.name === "write_file" ||
            seg.name === "edit_file" ||
            seg.name === "delete_file" ||
            seg.name === "rename_file" ||
            seg.name === "restore_file" ||
            seg.name === "project_write" ||
            seg.name === "project_edit" ||
            seg.name === "project_delete" ||
            seg.name === "project_rename" ||
            seg.name === "project_restore"
          ) {
            void refreshProjectFiles();
            void refreshModifiedFiles();
            if (viewingNote) void loadFileDiff(viewingNote);
          } else if (seg.name === "md_write" || seg.name === "md_edit") {
            if (sessionRoot) {
              void refreshProjectDocs();
              void refreshProjectFiles();
              void refreshModifiedFiles();
            } else {
              void refreshNotes();
            }
          }
        }
      } else if (event.type === "tool_image") {
        const idx = state.toolSegIdx[event.id];
        const seg = idx !== undefined ? state.segs[idx] : undefined;
        if (seg && seg.kind === "tool") {
          seg.image = event.image;
          state.commit();
        }
      } else if (event.type === "run_code") {
        const out = await runPython(event.code);
        resolveCode(event.id, out).catch(() => {});
      } else if (event.type === "tool_approval") {
        state.commit();
        setPendingEdit({
          id: event.id,
          tool: event.tool,
          path: event.path,
          diff: event.diff,
          warning: event.warning,
          command: event.command,
        });
      } else if (event.type === "error") {
        state.commit();
        setError(event.message);
      } else if (event.type === "done") {
        state.commit();
        state.onDone?.(event.finish_reason);
      }
    };
  };

  const turnBusyRef = useRef(false);

  async function runTurn(history: Message[], initialSegments: Segment[] = []) {
    if (busy || !model) {
      turnBusyRef.current = false;
      return;
    }
    turnBusyRef.current = true;
    setError(null);
    // If the session project folder is missing, bail immediately with a helpful error.
    if (view === "code" && sessionRoot && isFolderMissing) {
      setError(
        `Project folder not found: ${sessionRoot}\nThe folder may have been moved, renamed, or deleted. Please relocate or choose a new folder before sending messages.`,
      );
      turnBusyRef.current = false;
      setBusy(false);
      return;
    }

    // A turn with no thread behind it used to stream, bill, render — and then be
    // dropped on the floor, because `persist` silently no-ops when there is no
    // thread. It is reachable by a single click on the Code tab with no coding
    // session, which leaves the composer live. Make somewhere to put it rather
    // than discarding work the user has already paid for.
    if (threadIdRef.current == null) {
      try {
        const id = await createThread();
        threadIdRef.current = id; // synchronous: this turn's persist needs it now
        await saveThreadConfig(
          id,
          JSON.stringify({
            added,
            made_in_chat: madeInChat,
            system_prompt: systemPrompt,
            system_scope: systemScope,
            project_root: sessionRoot ?? undefined,
            compaction: compaction ?? undefined,
            tools: {
              toolsOn,
              webSearchOn,
              webFetchOn,
              webOn,
              visionOn,
              godotToolsOn,
              browserOn,
              liteTools,
              codeGroupsOn,
              mdWriteOn,
              notesOn,
              htmlWriteOn,
              codeOn,
              samplingOn,
              temperature,
              topP,
              maxTokens,
            },
          }),
        ).catch(() => {});
        hydratedThread.current = id;
        selectThreadId(id);
        setThreads(await listThreads());
      } catch (e) {
        setError(`Couldn't start a chat to save this in: ${e}`);
        turnBusyRef.current = false;
        setBusy(false);
        return;
      }
    }
    stickRef.current = true;
    setMessages(history);
    setBusy(true);
    setSegments(initialSegments);
    setPendingEdit(null);

    let acc = "";
    let racc = "";
    // The turn's render timeline, built in event order. `acc`/`racc` stay as the
    // flattened text/reasoning for the persisted message's content/reasoning
    // fields (what the model, copy, and export consume); `segs` is the interleaved
    // view. Mutated in place, then re-published by reference so React re-renders.
    const segs: Segment[] = [...initialSegments];
    const toolSegIdx: Record<string, number> = {};
    for (let i = 0; i < segs.length; i++) {
      const s = segs[i];
      if (s.kind === "tool" && s.id) {
        toolSegIdx[s.id] = i;
      }
    }
    // Coalesce high-frequency delta/reasoning segment renders to animation frames
    // (~60Hz) while keeping `acc`/`racc`/`segs` mutations synchronous for tool
    // boundaries and turn persistence.
    let rafId: number | null = null;
    const cancelCommit = () => {
      if (rafId !== null && typeof cancelAnimationFrame === "function") {
        cancelAnimationFrame(rafId);
        rafId = null;
      }
    };
    const commit = () => {
      cancelCommit();
      setSegments(segs.slice());
    };
    const scheduleCommit = () => {
      if (typeof requestAnimationFrame !== "function") {
        setSegments(segs.slice());
        return;
      }
      if (rafId === null) {
        rafId = requestAnimationFrame(() => {
          rafId = null;
          setSegments(segs.slice());
        });
      }
    };
    if (segs.length > 0) commit();
    const appendText = (kind: "text" | "reasoning", text: string) => {
      const last = segs[segs.length - 1];
      if (last && last.kind === kind) last.text += text;
      else segs.push({ kind, text });
      scheduleCommit();
    };
    try {
      if (samplingOn) {
        console.log("[sampling] Sent to model:", samplingParams());
      }
      // Composed immutably, and this is load-bearing, not style. `forModel`
      // returns the array it was handed whenever compaction
      // stands down — and on this path it is handed `history`, which is the
      // array `final` below spreads. Pushing the staged @-attach reads into it
      // mutated `history` in place, so `final` picked up a leading empty
      // assistant turn carrying nothing but the read chip: one bubble for the
      // chip, one for the answer. (Submission guards could never have caught it —
      // there was only ever one send, and one persist, per turn.)
      // The files staged for this turn ride along so `forModel` can attribute
      // the reads to this message. Derived from the segments rather than read
      // off `stagedFiles`: onSend clears that state before the turn starts, and
      // regenerate() reuses the prior turn's chips rather than any state.
      const stagedNames = initialSegments.flatMap((s) => {
        if (s.kind !== "tool") return [];
        try {
          const path = (JSON.parse(s.args ?? "{}") as { path?: unknown }).path;
          return typeof path === "string" && path ? [path] : [];
        } catch {
          return [];
        }
      });
      const modelMessages = [
        ...forModel(history, stagedNames),
        ...(initialSegments.length > 0
          ? [
              {
                role: "assistant" as const,
                content: "",
                segments: initialSegments,
                created_at: Date.now(),
              },
            ]
          : []),
      ];
      const toolsToSend = enabledToolList();
      if (!sessionRoot && initialSegments.some((s) => s.kind === "tool" && s.name === "read_note") && !toolsToSend.includes("read_note")) {
        toolsToSend.push("read_note");
      }
      if (sessionRoot && initialSegments.some((s) => s.kind === "tool" && (s.name === "read_file" || s.name === "project_read")) && !toolsToSend.includes("read_file")) {
        toolsToSend.push("read_file");
      }
      let lastFinishReason: string | null = null;
      await sendChat(
        {
          model,
          messages: modelMessages,
          thinking_effort: thinkingEffort,
          reasoning_style: activeConnDef().reasoning ?? null,
          max_context_tokens: contextCap(),
          max_tool_calls: toolCallCap(),
          ...samplingParams(),
        },
        toolsToSend,
        contextFileList(),
        systemForModel(),
        profileForModel(),
        connRoute(),
        sessionRoot,
        visionOn,
        makeHandler({
          acc: () => acc,
          setAcc: (s) => (acc = s),
          racc: () => racc,
          setRacc: (s) => (racc = s),
          segs,
          appendText,
          toolSegIdx,
          commit,
          onDone: (reason) => {
            lastFinishReason = reason;
          },
        }),
        readPruneTurns,
      );
      // Did this turn leave the model's own mark on the transcript?
      // `acc`/`racc` mirror every text and reasoning segment, so they carry the
      // prose. The segment test is what keeps a turn that only ever made tool
      // calls from vanishing — but it must count REAL calls: `call_init_*` chips
      // are the @-attach reads staged by onSend, not model output. Counting either of those
      // persisted an empty assistant bubble when a turn died before generating a
      // token.
      const generatedContent =
        acc.trim().length > 0 ||
        racc.trim().length > 0 ||
        segs.some((s) => s.kind === "tool" && !s.id?.startsWith("call_init_"));
      const final = generatedContent
        ? [
            ...history,
            {
              role: "assistant" as const,
              content: acc,
              reasoning: racc || undefined,
              segments: segs.length ? segs : undefined,
              created_at: Date.now(),
            },
          ]
        : history;
      if (generatedContent) {
        cancelCommit();
        setMessages(final);
        setSegments([]);
        setPendingTool(null);
      } else {
        cancelCommit();
        setSegments([]);
        setPendingTool(null);
        if (lastFinishReason !== "cancelled" && !isStopping) {
          setNotice("The model returned an empty response — resend to try again.");
        }
      }
      await persist(final);
    } catch (e) {
      setError(String(e));
      // Don't drop a long turn's work when it dies mid-loop (e.g. a context-overflow
      // 500 on a later iteration): persist whatever reasoning/text/tool calls
      // accumulated, so the transcript and the record of what ran survive. The file
      // edits themselves already landed on disk via their approvals.
      // Same test as the success path above, and for the same reason: a turn that
      // died before emitting a token has nothing of the model's to keep.
      const generatedContent =
        acc.trim().length > 0 ||
        racc.trim().length > 0 ||
        segs.some((s) => s.kind === "tool" && !s.id?.startsWith("call_init_"));
      if (generatedContent) {
        const partial = [
          ...history,
          {
            role: "assistant" as const,
            content: acc,
            reasoning: racc || undefined,
            segments: segs.length ? segs : undefined,
            created_at: Date.now(),
          },
        ];
        setMessages(partial);
        await persist(partial).catch(() => {});
      }
    } finally {
      cancelPython();
      cancelCommit();
      setSegments([]);
      setPendingEdit(null);
      setPendingTool(null);
      setBusy(false);
      turnBusyRef.current = false;
    }
  }



  const onSendRef = useRef(onSend);
  useEffect(() => { onSendRef.current = onSend; });
  const viewRef = useRef(view);
  useEffect(() => { viewRef.current = view; });
  const isCompactingRef = useRef(isCompacting);
  useEffect(() => { isCompactingRef.current = isCompacting; });
  const sessionRootRef = useRef(sessionRoot);
  useEffect(() => { sessionRootRef.current = sessionRoot; });

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let mounted = true;
    import("@tauri-apps/api/event").then(({ listen }) => {
      listen("process_crash", (e: any) => {
        const { id, code, tail, root } = e.payload as { id: number, code: number, tail: string, root?: string };
        if (viewRef.current !== "code") return;
        if (isCompactingRef.current) return;
        if (root && sessionRootRef.current !== root) return;
        console.log("Process crashed:", e.payload);
        const text = `[SYSTEM MESSAGE: Background Process Crashed]\nThe background process (Task ${id}) exited with error code ${code}.\n\nLast logs:\n\`\`\`\n${tail}\n\`\`\`\n\nPlease review these logs and fix the issue.`;
        onSendRef.current(text);
      }).then(u => {
        if (!mounted) u();
        else unlisten = u;
      });
    });
    return () => {
      mounted = false;
      if (unlisten) unlisten();
    };
  }, []);

  // Auto-dismiss transient errors after 7 seconds
  useEffect(() => {
    if (!error) return;
    const timer = setTimeout(() => {
      setError(null);
    }, 7000);
    return () => clearTimeout(timer);
  }, [error, setError]);

  // Auto-dismiss transient notices after 7 seconds (never while compacting)
  useEffect(() => {
    if (!notice || isCompacting) return;
    const timer = setTimeout(() => {
      setNotice(null);
    }, 7000);
    return () => clearTimeout(timer);
  }, [notice, isCompacting]);

  function onSend(text: string) {
    if (turnBusyRef.current || busy || !model) return;
    if (!text && images.length === 0 && stagedFiles.length === 0 && stagedExternalFiles.length === 0) return;
    turnBusyRef.current = true;
    setBusy(true);
    const imgs = images;
    setImages([]);
    // Compaction: one command, one note. `/compact` folds the prior note and the
    // new transcript into a single handover note; the model carries decisions
    // forward by copying them. There are no modes — the budget (or its absence)
    // decides how much raw transcript is shown, never what is remembered.
    if (/^\/compact(\s|$)/i.test(text)) {
      turnBusyRef.current = false;
      setBusy(false);
      runCompaction();
      return;
    }
    // Clear the live transcript to an empty slate, but KEEP the compaction note
    // (so the model still has the handover context). This is the "clean slate" that was missing
    // — the only previous path was deleting messages one by one. /uncompact
    // already handles clearing compaction state, so /clear doesn't duplicate that.
    if (/^\/clear\s*$/i.test(text)) {
      turnBusyRef.current = false;
      setBusy(false);
      if (messages.length === 0 && !compaction) {
        setError("Nothing to clear — the transcript is already empty.");
        return;
      }
      const had = messages.length;
      const hadNote = !!compaction;
      setMessages([]);
      setSegments([]);
      if (compaction && compaction.up_to !== 0) {
        setCompaction({ ...compaction, up_to: 0 });
      }
      // Persist the wipe immediately — otherwise switching chats (or relaunching)
      // reloads the thread from the store, where the old transcript still lives,
      // and the "cleared" messages come back.
      void persist([]);
      setNudgeDismissed(false);
      setNotice(
        hadNote
          ? `Transcript cleared (${had} messages). Compaction note preserved — the model still receives the handover context.`
          : `Transcript cleared (${had} messages).`,
      );
      return;
    }
    // Compaction only ever changed what gets SENT (see `forModel`) — the transcript
    // was never touched — so undoing it is just dropping the note. Works retroactively
    // on any thread compacted before this command existed.
    if (/^\/uncompact\s*$/i.test(text)) {
      turnBusyRef.current = false;
      setBusy(false);
      if (!compaction) {
        setError("Nothing to undo — this thread isn't compacted.");
        return;
      }
      const restored = compaction.up_to;
      setCompaction(null);
      setNudgeDismissed(false);
      setNotice(
        `Handover note discarded — all ${restored} earlier messages are being sent in full again.`,
      );
      return;
    }
    // Pre-compute vector embeddings for the active project repository (/index)
    if (/^\/index\s*$/i.test(text)) {
      turnBusyRef.current = false;
      setBusy(false);
      if (!sessionRoot) {
        setError("Open a project folder first to index its code.");
        return;
      }
      startIndexing();
      return;
    }
    const filesToRead = [...stagedFiles];
    const externalFilesToRead = [...stagedExternalFiles];
    setStagedFiles([]);
    setStagedExternalFiles([]);

    void (async () => {
      try {
        const MAX_STAGED_CHARS = 32_000;
        const capStagedContent = (raw: string) => {
          if (raw.length <= MAX_STAGED_CHARS) return raw;
          const totalLines = raw.split("\n").length;
          return (
            `${raw.slice(0, MAX_STAGED_CHARS)}\n\n` +
            `[content truncated at ${MAX_STAGED_CHARS} characters (of ${totalLines} lines) — pass start and lines to read_file to inspect specific ranges]`
          );
        };

        const initialSegments: Segment[] = [];
        if (filesToRead.length > 0) {
          for (let i = 0; i < filesToRead.length; i++) {
            const filePath = filesToRead[i];
            let content = "";
            try {
              if (sessionRoot) {
                content = await readProjectFile(sessionRoot, filePath);
              } else {
                content = await readNote(filePath);
              }
            } catch (err) {
              content = `Error reading file: ${err}`;
            }
            const toolName = sessionRoot ? "read_file" : "read_note";
            const args = JSON.stringify({ path: filePath });
            initialSegments.push({
              kind: "tool",
              id: `call_init_${Date.now()}_${i}`,
              name: toolName,
              args,
              title: toolTitle(toolName, args),
              output: capStagedContent(content),
            });
          }
        }
        if (externalFilesToRead.length > 0) {
          const offset = filesToRead.length;
          for (let j = 0; j < externalFilesToRead.length; j++) {
            const ext = externalFilesToRead[j];
            const toolName = sessionRoot ? "read_file" : "read_note";
            const args = JSON.stringify({ path: ext.name });
            initialSegments.push({
              kind: "tool",
              id: `call_init_ext_${Date.now()}_${offset + j}`,
              name: toolName,
              args,
              title: toolTitle(toolName, args),
              output: capStagedContent(ext.content),
            });
          }
        }
        const actionMatch = /^\/([a-z0-9_-]+)(?:\s+([\s\S]*))?$/i.exec(text.trim());
        const matchedAction = actionMatch
          ? actions.find((a) => a.name.toLowerCase() === actionMatch[1].toLowerCase())
          : null;

        await runTurn(
          [
            ...messages,
            {
              role: "user",
              content: text,
              images: imgs,
              created_at: Date.now(),
              ...(matchedAction
                ? {
                    action: {
                      name: matchedAction.name,
                      description: matchedAction.description,
                      template: matchedAction.template,
                    },
                  }
                : {}),
            },
          ],
          initialSegments,
        );
      } catch (err) {
        turnBusyRef.current = false;
        setBusy(false);
        setError(`Failed to start turn: ${err}`);
      }
    })();
  }

  function regenerate() {
    if (turnBusyRef.current || busy) return;
    let lastUser = -1;
    for (let i = messages.length - 1; i >= 0; i--) {
      if (messages[i].role === "user") {
        lastUser = i;
        break;
      }
    }
    if (lastUser >= 0) {
      turnBusyRef.current = true;
      setBusy(true);
      if (compaction && compaction.up_to > lastUser) {
        setCompaction({ ...compaction, up_to: lastUser });
      }
      const priorAssistant = messages[lastUser + 1];
      const initialSegs = priorAssistant?.segments?.filter(
        (s) => s.kind === "tool" && s.id?.startsWith("call_init_"),
      );
      void runTurn(messages.slice(0, lastUser + 1), initialSegs ?? []);
    }
  }

  async function stop() {
    try {
      cancelPython();
      if (pendingEdit) {
        resolveEdit(pendingEdit.id, "reject:Turn stopped by user").catch(() => {});
        setPendingEdit(null);
      }
      // `true` = a live turn's token was cancelled. Only then latch the
      // "Stopping…" state: a stop that found nothing in flight (e.g. clicked
      // during the preflight window before send_chat registers the token) must
      // not disable the Stop button for the turn that is about to start.
      if (await stopTurn()) setIsStopping(true);
    } catch {
      /* ignore */
    }
  }

  // Save an edited message. Two modes, by role:
  //  - user: truncate everything after it, replace the text, and re-run from that
  //    point (editable history, spec §5) — the long-standing behaviour.
  //  - assistant: edit IN PLACE with NO rerun (opt-in via the edit_messages pref).
  //    Rewrites the stored `content` — which is exactly what gets re-sent to the
  //    model next turn — so you can scrub a line that would otherwise pollute the
  //    rest of the chat, without paying to regenerate a turn that did real work.
  //    created_at is kept: it's the same message, corrected, not a new one.
  function saveEdit(i: number) {
    if (messages[i].role === "assistant") {
      const next = messages.map((m, idx) =>
        idx === i
          ? {
              ...m,
              content: editDraft,
              // A segmented turn renders from segments, so the edit has to land
              // there too or nothing visibly changes (see withEditedText).
              segments: m.segments?.length
                ? withEditedText(m.segments, editDraft)
                : m.segments,
            }
          : m,
      );
      setEditingIdx(null);
      setEditDraft("");
      setMessages(next);
      void persist(next);
      return;
    }
    if (turnBusyRef.current || busy) return;
    const text = editDraft.trim();
    if (!text) return;
    setEditingIdx(null);
    setEditDraft("");
    const edited = { ...messages[i], content: text, created_at: Date.now() };
    turnBusyRef.current = true;
    setBusy(true);
    if (compaction && compaction.up_to > i) {
      setCompaction({ ...compaction, up_to: i });
    }
    void runTurn([...messages.slice(0, i), edited]);
  }

  async function deleteMessage(i: number) {
    if (busy) return;
    // Cancel the editor for ANY delete at or before it, not just its own index.
    // Removing an earlier message shifts every later one down, so an open editor
    // keeps a now-stale index: Save would overwrite a different message, and on
    // the user-message branch it truncates the thread at the drifted index and
    // re-runs a real turn against a history that was silently corrupted.
    if (editingIdx !== null && editingIdx >= i) cancelEdit();
    const next = messages.filter((_, idx) => idx !== i);
    // The compaction boundary counts messages, so it has to move too:
    // 1. Deleting inside the folded prefix (i < up_to) shifts the fold boundary down by 1.
    //    Crucially, reaching 0 does NOT nullify compaction — only /uncompact does that.
    //    When up_to reaches 0, the handover note acts as a standing context note for
    //    history that has been cleared or deleted from the transcript.
    // 2. Deleting in the tail (i >= up_to) shrinks the total transcript, so up_to
    //    must be clamped to next.length so it never extends past the surviving messages.
    if (compaction) {
      const shifted = i < compaction.up_to ? compaction.up_to - 1 : compaction.up_to;
      const clamped = Math.min(Math.max(0, shifted), next.length);
      if (clamped !== compaction.up_to) {
        setCompaction({ ...compaction, up_to: clamped });
      }
    }
    setMessages(next);
    await persist(next);
  }




  function cancelModal() {
    if (!modal) return;
    if (modal.kind === "confirm") modal.resolve(false);
    else modal.resolve(null);
    setModal(null);
  }

  function okModal() {
    if (!modal) return;
    if (modal.kind === "confirm") modal.resolve(true);
    else modal.resolve(modal.value);
    setModal(null);
  }

  const appContextValue = {
    availableUpdate,
    setAvailableUpdate,
    viewingNote,
    viewerKind,
    noteContent,
    noteEditing,
    noteDraft,
    noteRaw,
    compactionMeta,
    stagedFiles,
    stageFile,
    unstageFile,
    toggleStagedFile,
    isStaged,
    stagedExternalFiles,
    stageExternalFile,
    unstageExternalFile,
    addExternalFiles,
    onCloseNote: closeNote,
    onRenameNote: renameNoteFromViewer,
    onDeleteNote: deleteNoteByName,
    onStartNoteEdit: startNoteEdit,
    onSaveNote: saveNote,
    onExportDocx: exportNoteDocx,
    setNoteEditing,
    setNoteRaw,
    setNoteDraft,
    sessionRoot,
    folderMissing: isFolderMissing,
    missingRoots,
    onChangeSessionRoot: handleChangeSessionRoot,
    notes,
    pages,
    projectDocs,
    projectFiles,
    expandedDirs,
    toggleDir,
    modifiedFiles,
    showDiff,
    setShowDiff,
    fileDiff,
    loadFileDiff,
    refreshModifiedFiles,
    added,
    madeInChat,
    compaction,
    openCompaction,
    openProjectDoc,
    openProjectFile,
    refreshProjectFiles,
    openNote,
    openPageInBrowser,
    openPageSource,
    deleteNoteByName,
    onRevealNote: (name: string) => { revealNote(name).catch((e) => setError(`Couldn't reveal: ${e}`)); },
    onRevealFolder: (path: string) => { revealFolder(path).catch((e) => setError(`Couldn't open folder: ${e}`)); },
    theme,
    setTheme,
    editMessages,
    setEditMessages,
    maxContext,
    setMaxContext,
    maxToolCalls,
    setMaxToolCalls,
    readPruneTurns,
    setReadPruneTurns,
    permAutoWrites,
    permAutoCommands,
    permWhitelist,
    permDraft,
    setPermDraft,
    updatePerms,
    addPermPattern,
    profileName,
    setProfileName,
    profilePreferred,
    setProfilePreferred,
    profileAbout,
    setProfileAbout,
    profileShare,
    setProfileShare,
    connBaseUrls,
    setConnBaseUrls,
    keyStatus,
    keyDraft,
    setKeyDraft,
    saveKey,
    deleteKey: clearKey,
    threads,
    setError,
    actions,
    refreshActions,
  };

  return (
    <AppContext.Provider value={appContextValue}>
    <div className="shell">
      <Sidebar
        view={view}
        busy={busy}
        threads={threads}
        currentThreadId={currentThreadId}
        menuThreadId={menuThreadId}
        menuUp={menuUp}
        trashed={trashed}
        showTrash={showTrash}
        missingRoots={missingRoots}
        profileName={profileName}
        profilePreferred={profilePreferred}
        onSwitchView={switchView}
        onNewChat={newChat}
        onNewSession={newSession}
        onSwitchThread={switchThread}
        onOpenMenu={(id, bottom) => {
          setMenuUp(bottom > window.innerHeight - 160);
          setMenuThreadId(menuThreadId === id ? null : id);
        }}
        onCloseMenu={() => setMenuThreadId(null)}
        onRenameThread={renameThreadById}
        onDuplicateThread={doDuplicate}
        onExportThread={openExportThreadById}
        onChangeThreadFolder={changeThreadFolder}
        onRemoveThread={removeThread}
        onToggleShowTrash={() => setShowTrash((s) => !s)}
        onRestoreTrash={restoreFromTrash}
        onPurgeTrash={purgeFromTrash}
        onEmptyTrash={emptyTrash}
        onOpenSettings={() => setShowSettings(true)}
      />

      <div className="main-split">
      <ChatSurface
        preview={preview}
        systemPrompt={systemPrompt}
        systemScope={systemScope}
        globalSystemChat={globalSystemChat}
        globalSystemCode={globalSystemCode}
        view={view}
        activeConn={activeConn}
        switchConnection={switchConnection}
        keyStatus={keyStatus}
        model={model}
        setModel={setModel}
        models={models}
        activeConnDef={activeConnDef}
        thinkingEffort={thinkingEffort}
        setThinkingEffort={setThinkingEffort}
        refreshModels={refreshModels}
        now={now}
        filesOpen={filesOpen}
        setFilesOpen={setFilesOpen}
        openFiles={openFiles}
        setShowInspector={setShowInspector}
        setShowSystem={setShowSystem}
        setDraftScope={setDraftScope}
        setSystemDraft={setSystemDraft}
        messages={messages}
        segments={segments}
        busy={busy}
        isStopping={isStopping}
        pendingTool={pendingTool}
        pendingEdit={pendingEdit}
        onAnswerEdit={answerEdit}
        editingIdx={editingIdx}
        editDraft={editDraft}
        setEditDraft={setEditDraft}
        copiedIdx={copiedIdx}
        editMessages={editMessages}
        scrollRef={scrollRef}
        contentRef={transcriptInnerRef}
        stickRef={stickRef}
        handleScroll={handleScroll}
        saveEdit={saveEdit}
        cancelEdit={cancelEdit}
        copyMessage={copyMessage}
        startEdit={startEdit}
        regenerate={regenerate}
        openExportMessage={openExportMessage}
        deleteMessage={deleteMessage}
        openNote={openNote}
        error={error}
        setError={setError}
        notice={notice}
        setNotice={setNotice}
        isCompacting={isCompacting}
        indexingStatus={indexingStatus}
        onStopIndexing={handleStopIndexing}
        onDismissIndexing={() => setIndexingStatus(null)}
        contextPressure={contextPressure}
        nudgeDismissed={nudgeDismissed}
        setNudgeDismissed={setNudgeDismissed}
        capForNudge={contextCap()}
        onSend={onSend}
        stop={stop}
        images={images}
        setImages={setImages}
        addImageFiles={addImageFiles}
        sessionRoot={sessionRoot}
        folderMissing={isFolderMissing}
        onChangeSessionRoot={handleChangeSessionRoot}
        fileInputRef={fileInputRef}
        toolsOn={toolsOn}
        setToolsOn={setToolsOn}
        webSearchOn={webSearchOn}
        setWebSearchOn={setWebSearchOn}
        webFetchOn={webFetchOn}
        setWebFetchOn={setWebFetchOn}
        webOn={webOn}
        setWebOn={setWebOn}
        visionOn={visionOn}
        godotToolsOn={godotToolsOn}
        setGodotToolsOn={setGodotToolsOn}
        setVisionOn={setVisionOn}
        browserOn={browserOn}
        setBrowserOn={setBrowserOn}
        mdWriteOn={mdWriteOn}
        setMdWriteOn={setMdWriteOn}
        notesOn={notesOn}
        setNotesOn={setNotesOn}
        htmlWriteOn={htmlWriteOn}
        setHtmlWriteOn={setHtmlWriteOn}
        codeOn={codeOn}
        setCodeOn={setCodeOn}
        samplingOn={samplingOn}
        setSamplingOn={setSamplingOn}
        temperature={temperature}
        setTemperature={setTemperature}
        topP={topP}
        setTopP={setTopP}
        maxTokens={maxTokens}
        setMaxTokens={setMaxTokens}
        showToggles={showToggles}
        setShowToggles={setShowToggles}
      />
      <TerminalPane layoutDir="row" />
      </div>

      <FilesDrawer
        filesOpen={filesOpen}
        filesWidth={filesWidth}
        onClose={() => setFilesOpen(false)}
        onStartResize={startFilesResize}
      />

      <input
        ref={fileInputRef}
        type="file"
        multiple
        style={{ display: "none" }}
        onChange={(e) => {
          if (!e.target.files || e.target.files.length === 0) return;
          if (sessionRoot) {
            void addExternalFiles(e.target.files);
            e.target.value = "";
          } else {
            handleUpload(e);
          }
        }}
      />

      <InstructionsModal
        showSystem={showSystem}
        onClose={() => setShowSystem(false)}
        draftScope={draftScope}
        setDraftScope={setDraftScope}
        globalSystemChat={globalSystemChat}
        globalSystemCode={globalSystemCode}
        systemPrompt={systemPrompt}
        systemDraft={systemDraft}
        setSystemDraft={setSystemDraft}
        setSystemScope={setSystemScope}
        setGlobalSystemChat={setGlobalSystemChat}
        setGlobalSystemCode={setGlobalSystemCode}
        setSystemPrompt={setSystemPrompt}
      />
      <SettingsModal
        showSettings={showSettings}
        settingsSection={settingsSection}
        setSettingsSection={setSettingsSection}
        onClose={() => setShowSettings(false)}
      />
      <ExportModal
        exportDoc={exportDoc}
        exportName={exportName}
        exportSaved={exportSaved}
        exportRaw={exportRaw}
        setExportName={setExportName}
        setExportRaw={setExportRaw}
        onClose={closeExport}
        onExport={doExport}
      />

      <ContextInspectorModal
        showInspector={showInspector}
        preview={preview}
        onClose={() => setShowInspector(false)}
      />

      <PromptModal
        modal={modal}
        setModal={setModal}
        okModal={okModal}
        cancelModal={cancelModal}
      />
    </div>
    </AppContext.Provider>
  );
}

export default App;
