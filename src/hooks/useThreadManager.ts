// ── surface: shared ──────────────────────────────────
// Thread/trash domain — switch/new/duplicate/remove/trash/restore/rename/export; optsRef pattern.
// Exports: useThreadManager, ThreadManagerOptions · Related: App.tsx, Sidebar.tsx

import {
  useState,
  useRef,
  useEffect,
  useCallback,
  type Dispatch,
  type SetStateAction,
} from "react";
import { open as pickDirectory } from "@tauri-apps/plugin-dialog";
import {
  listThreads,
  createThread,
  loadThread,
  saveThread,
  duplicateThread,
  deleteThread,
  renameThread,
  listTrashed,
  restoreThread,
  purgeThread,
  purgeTrashed,
  loadThreadConfig,
  saveThreadConfig,
  exportMarkdown,
  type ThreadInfo,
  type Message,
} from "../lib/provider";
import {
  deriveTitle,
  slugify,
  FULL_COPY,
  CHAT_TOOLS_DEFAULT,
  SESSION_TOOLS_DEFAULT,
} from "../utils";

export interface ThreadManagerOptions {
  busy: boolean; // a turn is running → every switch/trash/rename/export action bails
  setError: (msg: string | null) => void; // error reporter
  clearTurnState: () => void; // reset turn state (busy, preview, …) on thread switch
  setMessages: Dispatch<SetStateAction<Message[]>>; // replace the transcript on thread switch
  sessionRoot: string | null; // current session root; null = chat surface
  setSessionRoot: Dispatch<SetStateAction<string | null>>; // setter — thread switch changes root
  view: "chat" | "code"; // current surface
  setView: Dispatch<SetStateAction<"chat" | "code">>; // surface switcher (bails while busy)
  confirmBox: (message: string, title: string) => Promise<boolean>; // confirm for delete/trash/purge
  promptBox: (title: string, initial: string) => Promise<string | null>; // rename prompt
  closeNote: () => void; // close the open note on thread switch
  refreshNotes: () => Promise<void>; // refresh the notes list on thread switch
}

// ── optsRef: the "latest props" pattern, and why it looks the way it does ──
// App's thread actions live in THIS hook but act on ~10 values that belong to
// App (setMessages, busy, sessionRoot, dialog callbacks). Threading
// all of them through every handler's signature would be noise (and the signature
// would change whenever App grows a flag); rebuilding handlers per render would
// hand every consumer fresh identities each render, killing memoization. So the
// handlers below are created once with stable identities and read the CURRENT
// values from optsRef at CALL time; App refreshes the ref every render via
// setOptions.
//
// The assignment is deliberately in the RENDER BODY, not a useEffect: a handler
// is invoked by a user event at any moment after a render commits, and an effect
// leaves a window (post-paint, pre-effect) where optsRef still holds LAST render's
// callbacks — a click in that window would act on a stale setMessages/sessionRoot/
// busy. Writing the ref during render guarantees it matches what's on screen
// before the browser can dispatch anything. This is React's documented "latest
// ref" exception to "don't write refs during render"; the docs show the guarded
// form (if prev !== next), the unconditional assignment here is the simpler
// variant.
//
// initialOpts exists for other callers (tests); App calls this with no argument,
// so in production the ref is born undefined and gets its first value from App's
// first render — safe, because a render always completes before any event fires.
export function useThreadManager(initialOpts?: ThreadManagerOptions) {
  const optsRef = useRef<ThreadManagerOptions | undefined>(initialOpts);
  if (initialOpts) optsRef.current = initialOpts;

  const setOptions = (opts: ThreadManagerOptions) => {
    optsRef.current = opts;
  };

  const [threads, setThreads] = useState<ThreadInfo[]>([]);
  const [trashed, setTrashed] = useState<ThreadInfo[]>([]);
  const [currentThreadId, setCurrentThreadId] = useState<number | null>(null);
  const [menuThreadId, setMenuThreadId] = useState<number | null>(null);

  const [exportDoc, setExportDoc] = useState<string | null>(null);
  const [exportName, setExportName] = useState("");
  const [exportSaved, setExportSaved] = useState<string | null>(null);
  const [exportRaw, setExportRaw] = useState(false);

  const threadIdRef = useRef<number | null>(null);

  useEffect(() => {
    threadIdRef.current = currentThreadId;
  }, [currentThreadId]);

  const refreshThreads = useCallback(async () => {
    try {
      const ts = await listThreads();
      setThreads(ts);
      return ts;
    } catch {
      return [];
    }
  }, []);

  const refreshTrashed = useCallback(async () => {
    try {
      const tr = await listTrashed();
      setTrashed(tr);
      return tr;
    } catch {
      return [];
    }
  }, []);

  async function persist(msgs: Message[]) {
    const { setError = () => {} } = optsRef.current ?? {};
    const id = threadIdRef.current ?? currentThreadId;
    if (id == null) return;
    try {
      await saveThread(id, deriveTitle(msgs), msgs);
      setThreads(await listThreads());
    } catch (e) {
      setError(`Save failed: ${e}`);
    }
  }

  async function switchThread(id: number) {
    const {
      busy = false,
      setError = () => {},
      clearTurnState = () => {},
      setMessages = () => {},
    } = optsRef.current ?? {};
    if (busy || id === currentThreadId) return;
    setError(null);
    clearTurnState();
    setCurrentThreadId(id);
    try {
      setMessages(await loadThread(id));
    } catch (e) {
      setError(`Couldn't load thread: ${e}`);
    }
  }

  async function newChat() {
    const {
      busy = false,
      setError = () => {},
      clearTurnState = () => {},
      setMessages = () => {},
    } = optsRef.current ?? {};
    if (busy) return;
    setError(null);
    try {
      const id = await createThread();
      // A new chat starts with its kind's tool defaults, not whatever the last
      // thread left switched on.
      await saveThreadConfig(
        id,
        JSON.stringify({ tools: CHAT_TOOLS_DEFAULT }),
      );
      clearTurnState();
      setCurrentThreadId(id);
      setMessages([]);
      setThreads(await listThreads());
    } catch (e) {
      setError(`Couldn't create chat: ${e}`);
    }
  }

  // A coding session is a thread bound to a project folder — its root goes into the
  // thread config, which turns on the project-file tools. Creating one is the
  // intentional start of coding.
  async function newSession() {
    const {
      busy = false,
      setError = () => {},
      clearTurnState = () => {},
      setSessionRoot = () => {},
      setMessages = () => {},
    } = optsRef.current ?? {};
    if (busy) return;
    let root: string | null = null;
    try {
      const picked = await pickDirectory({
        directory: true,
        title: "Choose a project folder for this coding session",
      });
      root = typeof picked === "string" ? picked.trim() : null;
    } catch (e) {
      setError(`Couldn't open the folder picker: ${e}`);
      return;
    }
    if (!root) return;
    setError(null);
    try {
      const id = await createThread();
      await saveThreadConfig(
        id,
        JSON.stringify({
          project_root: root,
          tools: SESSION_TOOLS_DEFAULT,
        }),
      );
      clearTurnState();
      setSessionRoot(root);
      setCurrentThreadId(id);
      setMessages([]);
      setThreads(await listThreads());
    } catch (e) {
      setError(`Couldn't create session: ${e}`);
    }
  }

  // Re-target a coding session's project folder without losing conversation history.
  async function changeThreadFolder(id: number) {
    const {
      busy = false,
      setError = () => {},
      setSessionRoot = () => {},
    } = optsRef.current ?? {};
    if (busy) return;
    let root: string | null = null;
    try {
      const picked = await pickDirectory({
        directory: true,
        title: "Choose a new project folder for this coding session",
      });
      root = typeof picked === "string" ? picked.trim() : null;
    } catch (e) {
      setError(`Couldn't open the folder picker: ${e}`);
      return;
    }
    if (!root) return;
    setError(null);
    try {
      const raw = await loadThreadConfig(id);
      const cfg = raw ? JSON.parse(raw) : {};
      cfg.project_root = root;
      if (!cfg.tools) {
        cfg.tools = SESSION_TOOLS_DEFAULT;
      }
      await saveThreadConfig(id, JSON.stringify(cfg));
      if (id === currentThreadId) {
        setSessionRoot(root);
      }
      setThreads(await listThreads());
    } catch (e) {
      setError(`Couldn't change project folder: ${e}`);
    }
  }

  // Chat and Code are separate spaces: switching views focuses the most-recent
  // thread of that kind, so code work never lands in a chat thread (or vice versa).
  function switchView(v: "chat" | "code") {
    const {
      busy = false,
      view = "chat",
      setView = () => {},
      sessionRoot = null,
      clearTurnState = () => {},
      setMessages = () => {},
      setSessionRoot = () => {},
    } = optsRef.current ?? {};
    if (v === view || busy) return;
    setView(v);
    const currentIsSession = sessionRoot != null;
    const wantSession = v === "code";
    if (currentIsSession === wantSession) return; // current thread already matches
    const target = threads.find((t) =>
      wantSession ? !!t.project_root : !t.project_root,
    );
    if (target) {
      switchThread(target.id);
    } else {
      // No thread of that kind yet — clear to an empty state.
      clearTurnState();
      setCurrentThreadId(null);
      setMessages([]);
      setSessionRoot(null);
    }
  }

  async function doDuplicate(id: number) {
    const {
      busy = false,
      setError = () => {},
      clearTurnState = () => {},
      setMessages = () => {},
    } = optsRef.current ?? {};
    if (busy) return;
    setError(null);
    try {
      const newId = await duplicateThread(id, FULL_COPY);
      clearTurnState();
      setCurrentThreadId(newId);
      setMessages(await loadThread(newId));
      setThreads(await listThreads());
    } catch (e) {
      setError(`Couldn't duplicate: ${e}`);
    }
  }

  async function removeThread(id: number) {
    const {
      busy = false,
      setError = () => {},
      closeNote = () => {},
      view = "chat",
      setMessages = () => {},
      setSessionRoot = () => {},
      clearTurnState = () => {},
    } = optsRef.current ?? {};
    if (busy) return;
    setError(null);
    try {
      await deleteThread(id);
      let ts = await listThreads();
      if (id === currentThreadId) {
        // The open thread is gone, so close the file viewer — whatever it was
        // showing belonged to the deleted context.
        closeNote();
        // Fall back to a thread of the SAME kind as the current view, so deleting
        // a chat never lands you on a code session (or vice versa) — which would
        // leave the file viewer stuck on the wrong surface via a stale sessionRoot.
        const isCode = view === "code";
        const sameKind = ts.filter((t) =>
          isCode ? !!t.project_root : !t.project_root,
        );
        if (sameKind.length > 0) {
          const next = sameKind[0];
          setCurrentThreadId(next.id);
          setMessages(await loadThread(next.id));
          setSessionRoot(next.project_root ?? null);
        } else if (isCode) {
          // No coding sessions left — clear to an empty, session-less state.
          setCurrentThreadId(null);
          setMessages([]);
          setSessionRoot(null);
        } else {
          const newId = await createThread();
          setCurrentThreadId(newId);
          setMessages([]);
          setSessionRoot(null);
          ts = await listThreads();
        }
        clearTurnState();
      }
      setThreads(ts);
      setTrashed(await listTrashed());
    } catch (e) {
      setError(`Couldn't delete: ${e}`);
    }
  }

  // Trash actions: restore brings a chat back to the list; purge removes it for
  // good. Only the irreversible purges keep a confirm — plain delete is undoable.
  async function restoreFromTrash(id: number) {
    const { setError = () => {} } = optsRef.current ?? {};
    setError(null);
    try {
      await restoreThread(id);
      setThreads(await listThreads());
      setTrashed(await listTrashed());
    } catch (e) {
      setError(`Restore failed: ${e}`);
    }
  }

  async function purgeFromTrash(id: number, title: string) {
    const {
      setError = () => {},
      confirmBox = async () => false,
    } = optsRef.current ?? {};
    if (
      !(await confirmBox(
        `Permanently delete "${title}"? This can't be undone.`,
        "Delete chat",
      ))
    )
      return;
    try {
      await purgeThread(id);
      setTrashed(await listTrashed());
    } catch (e) {
      setError(`Couldn't delete: ${e}`);
    }
  }

  async function emptyTrash() {
    const {
      setError = () => {},
      confirmBox = async () => false,
    } = optsRef.current ?? {};
    if (
      !(await confirmBox(
        `Permanently delete all ${trashed.length} chats in the trash? This can't be undone.`,
        "Empty trash",
      ))
    )
      return;
    try {
      await purgeTrashed();
      setTrashed(await listTrashed());
    } catch (e) {
      setError(`Couldn't empty trash: ${e}`);
    }
  }

  async function renameThreadById(id: number, current: string) {
    const { setError = () => {}, promptBox = async () => null } =
      optsRef.current ?? {};
    const next = await promptBox("Rename chat to:", current);
    if (next == null) return;
    const trimmed = next.trim();
    if (!trimmed || trimmed === current) return;
    try {
      await renameThread(id, trimmed);
      setThreads(await listThreads());
    } catch (e) {
      setError(`Rename failed: ${e}`);
    }
  }

  // Assemble the whole thread into a portable markdown document.
  function threadMarkdown(msgs: Message[], title: string): string {
    const lines: string[] = [`# ${title}`, ""];
    for (let i = 0; i < msgs.length; i++) {
      const m = msgs[i];
      if (m.role === "user") {
        const next = msgs[i + 1];
        const attachedNames: string[] = [];
        if (next?.segments) {
          for (const s of next.segments) {
            if (
              s.kind === "tool" &&
              (s.name === "read_note" || s.name === "read_file" || s.name === "project_read") &&
              s.id?.startsWith("call_init_") &&
              s.args
            ) {
              try {
                const path = (JSON.parse(s.args) as { path?: string }).path;
                if (path) attachedNames.push(path);
              } catch {}
            }
          }
        }

        lines.push("**You**", "");
        if (attachedNames.length > 0) {
          lines.push(`*[Attached: ${attachedNames.join(", ")}]*`, "");
        }
        if (m.content) {
          lines.push(m.content, "");
        }
      } else if (m.role === "assistant") {
        lines.push("**Assistant**", "", m.content, "");
      }
    }
    return lines.join("\n").trim() + "\n";
  }

  // Export any thread (current or not) by loading its messages from the store, so
  // the action can live in the per-thread sidebar menu rather than a top-bar button.
  async function openExportThreadById(id: number, title: string) {
    const msgs = await loadThread(id);
    if (!msgs.length) return;
    const name = title || deriveTitle(msgs);
    setExportDoc(threadMarkdown(msgs, name));
    setExportName(`${slugify(name)}.md`);
    setExportSaved(null);
    setExportRaw(false);
  }

  function openExportMessage(content: string, i: number) {
    setExportDoc(content.trim() + "\n");
    setExportName(`reply-${i + 1}.md`);
    setExportSaved(null);
    setExportRaw(false);
  }

  function closeExport() {
    setExportDoc(null);
    setExportSaved(null);
  }

  async function doExport() {
    const { setError = () => {}, refreshNotes = async () => {} } =
      optsRef.current ?? {};
    if (exportDoc == null) return;
    try {
      const path = await exportMarkdown(exportName, exportDoc);
      setExportSaved(path);
      refreshNotes();
    } catch (e) {
      setError(`Export failed: ${e}`);
    }
  }

  return {
    threads,
    setThreads,
    trashed,
    setTrashed,
    currentThreadId,
    setCurrentThreadId,
    menuThreadId,
    setMenuThreadId,
    refreshThreads,
    refreshTrashed,
    exportDoc,
    setExportDoc,
    exportName,
    setExportName,
    exportSaved,
    setExportSaved,
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
    threadMarkdown,
    openExportThreadById,
    openExportMessage,
    closeExport,
    doExport,
    setOptions,
  };
}
