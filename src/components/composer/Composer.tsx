// ── surface: chat ──────────────────────────────────
// Message input ("the chat box") — used by both chat and code views; slash-command hints,
// keyboard-driven @ mentions, drag-and-drop file staging, and ephemeral turn attachments.
// Exports: Composer · Related: ChatSurface.tsx, useTurnRunner.ts, utils.ts (SLASH_COMMANDS)

import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { SLASH_COMMANDS } from "../../utils";
import { useAppContext } from "../../AppContext";
import { FilesIcon } from "../icons";

export function Composer({
  onSubmit,
  busy,
  isStopping,
  model,
  stop,
  controls,
  images,
  onAddImages,
  onRemoveImage,
}: {
  onSubmit: (text: string) => void;
  busy: boolean;
  /** Stop acknowledged a live turn — the Stop button renders disabled as
   *  "Stopping…" until the turn ends, so the click gets instant feedback. */
  isStopping: boolean;
  model: string;
  stop: () => void;
  controls: ReactNode;
  /** Images attached to the next send (base64 data URIs), owned by App so the
   *  tools-popup "Upload file" button can add to them. Rendered here as thumbs. */
  images: string[];
  onAddImages: (files: File[]) => void;
  onRemoveImage: (index: number) => void;
}) {
  const {
    stagedFiles,
    stageFile,
    unstageFile,
    stagedExternalFiles,
    unstageExternalFile,
    addExternalFiles,
    projectFiles,
    notes,
    pages,
    sessionRoot,
    actions,
  } = useAppContext();

  const [input, setInput] = useState("");
  const [cursorPos, setCursorPos] = useState<number | null>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const activeItemRef = useRef<HTMLButtonElement | null>(null);
  const [slashIndex, setSlashIndex] = useState(0);
  const [atIndex, setAtIndex] = useState(0);
  const [atDismissed, setAtDismissed] = useState(false);
  const [isDragOver, setIsDragOver] = useState(false);
  const isSubmittingRef = useRef(false);

  // No dependency array, deliberately — this effect must run after EVERY render.
  //
  // The lock exists to swallow a second submit fired in the same tick as the
  // first (form onSubmit plus a submit button's default action). The real "a turn
  // is running" lock is the synchronous `turnBusyRef` in App's onSend, which is
  // set before its first await. Releasing this one on a `busy` TRANSITION made it
  // unreleasable instead: the slash commands (/clear, /uncompact, /consume, and
  // /compact whenever it early-returns) finish entirely inside onSend without ever
  // touching `busy`, so the transition this effect waited for never arrived — the
  // ref stayed true, Send looked enabled but no-opped, and Enter did nothing until
  // some unrelated action started a real turn.
  //
  // A release per render cannot reopen the double-submit window: the same-tick
  // dedup it guards completes before React re-renders, and turnBusyRef covers the
  // async window that follows.
  useEffect(() => {
    if (!busy) {
      isSubmittingRef.current = false;
    }
  });

  // Slash commands (built-in commands + dynamic prompt actions)
  const slashQuery = /^\/([a-z0-9_-]*)$/i.exec(input.trimStart())?.[1]?.toLowerCase();
  const allSlashCommands = useMemo(() => {
    const builtins = SLASH_COMMANDS.map((c) => ({
      ...c,
      isAction: false,
    }));
    const actionItems = (actions || []).map((a) => ({
      name: a.name,
      arg: "[prompt]",
      hint: a.description,
      isAction: true,
    }));
    return [...builtins, ...actionItems];
  }, [actions]);

  const slashMatches =
    slashQuery === undefined
      ? []
      : allSlashCommands.filter((c) => c.name.startsWith(slashQuery));
  const slashOpen = slashMatches.length > 0;
  const activeSlash = slashMatches[Math.min(slashIndex, Math.max(0, slashMatches.length - 1))];

  // @ Mention autocompletion
  const pos = cursorPos ?? input.length;
  const textBefore = input.slice(0, pos);
  const atMatch = /(?:^|\s)@([^\s@]*)$/.exec(textBefore);
  const atQuery = atMatch !== null ? atMatch[1].toLowerCase() : null;
  const atOpen = atQuery !== null && !slashOpen && !atDismissed;

  const candidates = useMemo(() => {
    if (sessionRoot) {
      return projectFiles;
    }
    return [...notes.map((n) => n.name), ...pages.map((p) => p.name)];
  }, [sessionRoot, projectFiles, notes, pages]);

  const atMatches = useMemo(() => {
    if (atQuery === null) return [];
    const q = atQuery.trim();
    if (!q) {
      return candidates.slice(0, 16);
    }
    const basenamePrefix: string[] = [];
    const basenameContains: string[] = [];
    const pathContains: string[] = [];
    for (const p of candidates) {
      const base = p.split("/").pop()?.toLowerCase() ?? "";
      const lower = p.toLowerCase();
      if (base.startsWith(q)) {
        basenamePrefix.push(p);
      } else if (base.includes(q)) {
        basenameContains.push(p);
      } else if (lower.includes(q)) {
        pathContains.push(p);
      }
    }
    return [...basenamePrefix, ...basenameContains, ...pathContains].slice(0, 16);
  }, [candidates, atQuery]);

  const activeAt = atMatches[Math.min(atIndex, Math.max(0, atMatches.length - 1))];

  useEffect(() => {
    setAtIndex(0);
  }, [atQuery]);

  useEffect(() => {
    if (atOpen && activeItemRef.current) {
      activeItemRef.current.scrollIntoView({ block: "nearest" });
    }
  }, [atIndex, atOpen]);

  function applySlash(name: string) {
    setInput(`/${name} `);
    setSlashIndex(0);
    inputRef.current?.focus();
  }

  function applyAt(path: string) {
    stageFile(path);
    if (atMatch) {
      const matchStart = atMatch.index + (atMatch[0].startsWith(" ") ? 1 : 0);
      const before = input.slice(0, matchStart);
      const after = input.slice(pos);
      // Consume the @query from composer text; file is staged in chip row above
      const cleanBefore = before;
      const cleanAfter =
        cleanBefore.endsWith(" ") && after.startsWith(" ")
          ? after.slice(1)
          : after;
      const next = `${cleanBefore}${cleanAfter}`;
      setInput(next);
      const newPos = cleanBefore.length;
      setCursorPos(newPos);
      setTimeout(() => {
        inputRef.current?.setSelectionRange(newPos, newPos);
        inputRef.current?.focus();
      }, 0);
    }
    setAtIndex(0);
    setAtDismissed(false);
  }

  useEffect(() => {
    const el = inputRef.current;
    if (el) {
      el.style.height = "auto";
      el.style.height = `${el.scrollHeight}px`;
    }
  }, [input]);

  function submit() {
    if (isSubmittingRef.current || busy || !model) return;
    const text = input.trim();
    if (!text && images.length === 0 && stagedFiles.length === 0 && stagedExternalFiles.length === 0) return;
    isSubmittingRef.current = true;
    setInput("");
    setCursorPos(0);
    onSubmit(text);
  }

  return (
    <form
      className="composer"
      onSubmit={(e) => {
        e.preventDefault();
        submit();
      }}
    >
      <div className="composer-inner">
        {slashOpen && (
          <div className="slash-menu" role="listbox">
            {slashMatches.map((c, i) => (
              <button
                key={c.name}
                type="button"
                role="option"
                aria-selected={c === activeSlash}
                className={`slash-item${c === activeSlash ? " active" : ""}`}
                onMouseDown={(e) => {
                  e.preventDefault();
                  applySlash(c.name);
                }}
                onMouseEnter={() => setSlashIndex(i)}
              >
                <span className="slash-name">
                  {c.isAction ? "⚡ /" : "/"}{c.name}
                  {c.arg && <span className="slash-arg"> {c.arg}</span>}
                </span>
                <span className="slash-hint">{c.hint}</span>
              </button>
            ))}
          </div>
        )}
        {atOpen && atMatches.length > 0 && (
          <div className="mention-menu" role="listbox">
            {atMatches.map((path, i) => {
              const parts = path.split("/");
              const filename = parts[parts.length - 1];
              const dir = parts.length > 1 ? parts.slice(0, -1).join("/") + "/" : "";
              const staged = stagedFiles.includes(path);
              return (
                <button
                  key={path}
                  ref={path === activeAt ? activeItemRef : undefined}
                  type="button"
                  role="option"
                  aria-selected={path === activeAt}
                  className={`mention-item${path === activeAt ? " active" : ""}`}
                  onMouseDown={(e) => {
                    e.preventDefault();
                    applyAt(path);
                  }}
                  onMouseEnter={() => setAtIndex(i)}
                >
                  <span className="mention-icon">
                    <FilesIcon />
                  </span>
                  <span className="mention-name">{filename}</span>
                  {dir && <span className="mention-dir">{dir}</span>}
                  {staged && <span className="mention-staged-badge">staged</span>}
                </button>
              );
            })}
          </div>
        )}
        <div
          className={`composer-box${isDragOver ? " drag-over" : ""}`}
          onDragOver={(e) => {
            if (
              e.dataTransfer.types.includes("application/x-outpost-file") ||
              e.dataTransfer.types.includes("Files")
            ) {
              e.preventDefault();
              e.dataTransfer.dropEffect = "copy";
              setIsDragOver(true);
            }
          }}
          onDragLeave={(e) => {
            if (!e.currentTarget.contains(e.relatedTarget as Node)) {
              setIsDragOver(false);
            }
          }}
          onDrop={(e) => {
            e.preventDefault();
            setIsDragOver(false);
            const outpostFile = e.dataTransfer.getData("application/x-outpost-file");
            if (outpostFile) {
              stageFile(outpostFile);
              return;
            }
            const plainText = e.dataTransfer.getData("text/plain");
            if (plainText && candidates.includes(plainText)) {
              stageFile(plainText);
              return;
            }
            const files = Array.from(e.dataTransfer.files);
            const imageFiles = files.filter((f) => f.type.startsWith("image/"));
            const otherFiles = files.filter((f) => !f.type.startsWith("image/"));
            if (imageFiles.length > 0) {
              onAddImages(imageFiles);
            }
            if (otherFiles.length > 0) {
              void addExternalFiles(otherFiles);
            }
          }}
        >
          {(stagedFiles.length > 0 || (stagedExternalFiles && stagedExternalFiles.length > 0)) && (
            <div className="composer-staged-files">
              {stagedFiles.map((path) => {
                const parts = path.split("/");
                const filename = parts[parts.length - 1];
                const dir = parts.length > 1 ? parts.slice(0, -1).join("/") + "/" : "";
                return (
                  <div key={path} className="staged-file-chip" title={path}>
                    <span className="staged-file-icon">
                      <FilesIcon />
                    </span>
                    {dir && <span className="staged-file-dir">{dir}</span>}
                    <span className="staged-file-name">{filename}</span>
                    <button
                      type="button"
                      className="staged-file-remove"
                      onClick={() => unstageFile(path)}
                      title="Remove from prompt"
                      aria-label={`Remove ${filename}`}
                    >
                      ×
                    </button>
                  </div>
                );
              })}
              {stagedExternalFiles?.map((f) => (
                <div
                  key={f.name}
                  className="staged-file-chip external"
                  title={f.path || f.name}
                >
                  <span className="staged-file-icon">
                    <FilesIcon />
                  </span>
                  <span className="staged-file-name">{f.name}</span>
                  <span className="staged-file-ext-badge">ext</span>
                  <button
                    type="button"
                    className="staged-file-remove"
                    onClick={() => unstageExternalFile(f.name)}
                    title="Remove from prompt"
                    aria-label={`Remove ${f.name}`}
                  >
                    ×
                  </button>
                </div>
              ))}
            </div>
          )}
          {images.length > 0 && (
            <div
              style={{
                display: "flex",
                gap: 8,
                flexWrap: "wrap",
                padding: "8px 4px 0",
              }}
            >
              {images.map((src, j) => (
                <div key={j} style={{ position: "relative" }}>
                  <img
                    src={src}
                    alt=""
                    style={{
                      width: 56,
                      height: 56,
                      objectFit: "cover",
                      borderRadius: 6,
                      display: "block",
                    }}
                  />
                  <button
                    type="button"
                    title="Remove"
                    aria-label="Remove image"
                    onClick={() => onRemoveImage(j)}
                    style={{
                      position: "absolute",
                      top: -6,
                      right: -6,
                      width: 18,
                      height: 18,
                      borderRadius: "50%",
                      border: "none",
                      cursor: "pointer",
                      padding: 0,
                      lineHeight: "16px",
                    }}
                  >
                    ×
                  </button>
                </div>
              ))}
            </div>
          )}
          <textarea
            ref={inputRef}
            className="composer-text"
            value={input}
            onChange={(e) => {
              setInput(e.target.value);
              setCursorPos(e.target.selectionStart);
              setAtDismissed(false);
            }}
            onClick={(e) => setCursorPos(e.currentTarget.selectionStart)}
            onKeyUp={(e) => setCursorPos(e.currentTarget.selectionStart)}
            onPaste={(e) => {
              const files = Array.from(e.clipboardData.items)
                .filter(
                  (it) => it.kind === "file" && it.type.startsWith("image/"),
                )
                .map((it) => it.getAsFile())
                .filter((f): f is File => !!f);
              if (files.length) {
                e.preventDefault();
                onAddImages(files);
              }
            }}
            onKeyDown={(e) => {
              if (slashOpen) {
                if (e.key === "ArrowDown") {
                  e.preventDefault();
                  setSlashIndex((i) => (i + 1) % slashMatches.length);
                  return;
                }
                if (e.key === "ArrowUp") {
                  e.preventDefault();
                  setSlashIndex(
                    (i) => (i - 1 + slashMatches.length) % slashMatches.length,
                  );
                  return;
                }
                if (e.key === "Tab" || (e.key === "Enter" && !e.shiftKey)) {
                  e.preventDefault();
                  if (activeSlash) applySlash(activeSlash.name);
                  return;
                }
                if (e.key === "Escape") {
                  e.preventDefault();
                  setInput("");
                  return;
                }
              }
              if (atOpen && atMatches.length > 0) {
                if (e.key === "ArrowDown") {
                  e.preventDefault();
                  setAtIndex((i) => (i + 1) % atMatches.length);
                  return;
                }
                if (e.key === "ArrowUp") {
                  e.preventDefault();
                  setAtIndex(
                    (i) => (i - 1 + atMatches.length) % atMatches.length,
                  );
                  return;
                }
                if (e.key === "Tab") {
                  e.preventDefault();
                  if (activeAt) applyAt(activeAt);
                  return;
                }
                if (e.key === "Escape") {
                  e.preventDefault();
                  setAtDismissed(true);
                  return;
                }
              }
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                e.stopPropagation();
                submit();
              }
            }}
            placeholder="Ask anything, @ to mention, / for actions"
            rows={1}
          />
          <div className="composer-controls">
            {controls}
            <div className="composer-actions">
              {busy ? (
                <button
                  type="button"
                  className="send-btn"
                  onClick={stop}
                  disabled={isStopping}
                  title={isStopping ? "Stopping…" : "Stop — abort this turn"}
                >
                  {isStopping ? "Stopping…" : "Stop"}
                </button>
              ) : (
                <button
                  type="submit"
                  className="send-btn"
                  disabled={
                    (!input.trim() &&
                      images.length === 0 &&
                      stagedFiles.length === 0) ||
                    !model
                  }
                >
                  Send
                </button>
              )}
            </div>
          </div>
        </div>
      </div>
    </form>
  );
}
