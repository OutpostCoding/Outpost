// ── surface: chat ──────────────────────────────────
// The turn surface — the NAME predates the code view: both chat and code views render
// turns through this component. Hosts the connection switcher, tools popup + lite panel.
// Exports: ChatSurface · Related: App.tsx, Composer.tsx, timeline/*

import React from "react";
import {
  CONNECTIONS,
  formatTime,
  toolLabel,
  type Connection,
} from "../../utils";
import type {
  ContextPreview,

  Message,
  Segment,
} from "../../lib/provider";
import { InstructionsIcon, RetryIcon, FilesIcon, CheckIcon, CopyIcon, EditIcon, DownloadIcon, TrashIcon } from "../icons";
import { Thinking } from "../timeline/Thinking";
import { TurnTimeline } from "../timeline/TurnTimeline";
import { Markdown } from "../markdown/Markdown";
import { CodeCell } from "../ui/CodeCell";
import { Sources } from "../timeline/Sources";
import { Composer } from "../composer/Composer";
import { Switch } from "../ui/Switch";
import { ParamSlider } from "../ui/ParamSlider";

export function ChatSurface({
  // Topbar
  preview,
  systemPrompt,
  systemScope,
  globalSystemChat,
  globalSystemCode,
  view,
  activeConn,
  switchConnection,
  keyStatus,
  model,
  setModel,
  models,
  activeConnDef,
  thinkingEffort,
  setThinkingEffort,
  refreshModels,
  now,
  filesOpen,
  setFilesOpen,
  openFiles,
  setShowInspector,
  setShowSystem,
  setDraftScope,
  setSystemDraft,

  // Timeline / Messages
  messages,
  segments,
  busy,
  pendingTool,
  pendingEdit,
  onAnswerEdit,
  editingIdx,
  editDraft,
  setEditDraft,
  copiedIdx,
  editMessages,
  scrollRef,
  contentRef,
  stickRef,
  handleScroll,
  saveEdit,
  cancelEdit,
  copyMessage,
  startEdit,
  regenerate,
  openExportMessage,
  deleteMessage,
  openNote,

  // Status / Notice
  error,
  setError,
  notice,
  setNotice,
  isCompacting = false,
  indexingStatus = null,
  onStopIndexing,
  onDismissIndexing,
  contextPressure,
  nudgeDismissed,
  setNudgeDismissed,
  capForNudge,

  // Composer & Toggles
  onSend,
  stop,
  isStopping,
  images,
  setImages,
  addImageFiles,
  sessionRoot,
  folderMissing = false,
  onChangeSessionRoot,

  fileInputRef,
  toolsOn,
  setToolsOn,
  webSearchOn,
  setWebSearchOn,
  webFetchOn,
  setWebFetchOn,
  webOn,
  setWebOn,
  visionOn,
  setVisionOn,
  godotToolsOn,
  setGodotToolsOn,
  browserOn,
  setBrowserOn,
  mdWriteOn,
  setMdWriteOn,
  notesOn,
  setNotesOn,
  htmlWriteOn,
  setHtmlWriteOn,
  codeOn,
  setCodeOn,
  samplingOn,
  setSamplingOn,
  temperature,
  setTemperature,
  topP,
  setTopP,
  maxTokens,
  setMaxTokens,
  showToggles,
  setShowToggles,
}: {
  preview: ContextPreview | null;
  systemPrompt: string;
  systemScope: "global" | "local";
  globalSystemChat: string;
  globalSystemCode: string;
  view: "chat" | "code";
  activeConn: string;
  switchConnection: (conn: string) => void;
  keyStatus: Record<string, boolean>;
  model: string;
  setModel: (m: string) => void;
  models: string[];
  activeConnDef: () => Connection;
  thinkingEffort: string;
  setThinkingEffort: (e: string) => void;
  refreshModels: () => void;
  now: string;
  filesOpen: boolean;
  setFilesOpen: React.Dispatch<React.SetStateAction<boolean>>;
  openFiles: () => void;
  setShowInspector: (v: boolean) => void;
  setShowSystem: (v: boolean) => void;
  setDraftScope: (s: "global_chat" | "global_code" | "local") => void;
  setSystemDraft: (d: string) => void;

  messages: Message[];
  segments: Segment[];
  busy: boolean;
  pendingTool: string | null;
  pendingEdit: any;
  onAnswerEdit?: (decision: "approve" | "reject" | "rename" | "remember", feedback?: string) => void;
  editingIdx: number | null;
  editDraft: string;
  setEditDraft: (d: string) => void;
  copiedIdx: number | null;
  editMessages: boolean;
  scrollRef: React.RefObject<HTMLDivElement | null>;
  contentRef: React.RefObject<HTMLDivElement | null>;
  stickRef: React.MutableRefObject<boolean>;
  handleScroll: () => void;
  saveEdit: (i: number) => void;
  cancelEdit: () => void;
  copyMessage: (text: string, i: number) => void;
  startEdit: (i: number) => void;
  regenerate: () => void;
  openExportMessage: (content: string, i: number) => void;
  deleteMessage: (i: number) => void;
  openNote: (name: string) => void;

  error: string | null;
  setError: (err: string | null) => void;
  notice: string | null;
  setNotice: (n: string | null) => void;
  isCompacting?: boolean;
  indexingStatus?: {
    active: boolean;
    current: number;
    total: number;
    done: boolean;
    message?: string;
  } | null;
  onStopIndexing?: () => void;
  onDismissIndexing?: () => void;
  contextPressure: boolean;
  nudgeDismissed: boolean;
  setNudgeDismissed: (d: boolean) => void;
  capForNudge: number | null;

  onSend: (text: string) => void;
  stop: () => void;
  /** Stop acknowledged a live turn — Stop buttons render as a disabled
   *  "Stopping…" until the turn's Done event lands. */
  isStopping: boolean;
  images: string[];
  setImages: React.Dispatch<React.SetStateAction<string[]>>;
  addImageFiles: (files: FileList | File[]) => void;
  sessionRoot: string | null;
  folderMissing?: boolean;
  onChangeSessionRoot?: () => void;

  fileInputRef: React.RefObject<HTMLInputElement | null>;
  toolsOn: boolean;
  setToolsOn: (v: boolean) => void;
  webSearchOn: boolean;
  setWebSearchOn: (v: boolean) => void;
  webFetchOn: boolean;
  setWebFetchOn: (v: boolean) => void;
  webOn: boolean;
  setWebOn: (v: boolean) => void;
  visionOn: boolean;
  setVisionOn: (v: boolean) => void;
  godotToolsOn: boolean;
  setGodotToolsOn: (v: boolean) => void;
  browserOn: boolean;
  setBrowserOn: (v: boolean) => void;
  mdWriteOn: boolean;
  setMdWriteOn: (v: boolean) => void;
  notesOn: boolean;
  setNotesOn: (v: boolean) => void;
  htmlWriteOn: boolean;
  setHtmlWriteOn: (v: boolean) => void;
  codeOn: boolean;
  setCodeOn: (v: boolean) => void;
  samplingOn: boolean;
  setSamplingOn: (v: boolean) => void;
  temperature: number;
  setTemperature: (v: number) => void;
  topP: number;
  setTopP: (v: number) => void;
  maxTokens: string;
  setMaxTokens: (v: string) => void;
  showToggles: boolean;
  setShowToggles: React.Dispatch<React.SetStateAction<boolean>>;
}) {
  const [copiedError, setCopiedError] = React.useState(false);

  return (
    <main className="app">
      <header className="topbar">
        <span className="brand">
          <svg className="brand-mark" viewBox="0 0 512 512" aria-hidden="true">
            <path
              fill="currentColor"
              fillRule="evenodd"
              d="M120 432 L158 158 L128 158 L128 120 L128 80 L188 80 L188 120 L226 120 L226 80 L286 80 L286 120 L324 120 L324 80 L384 80 L384 120 L384 158 L354 158 L392 432 Z M221 278 L221 218 Q221 206 256 206 Q291 206 291 218 L291 278 Z"
            />
          </svg>
          Outpost
        </span>
        <button
          className="inspect-btn"
          onClick={() => setShowInspector(true)}
          title="Inspect the exact context sent to the model"
        >
          {preview ? `${preview.total_tokens.toLocaleString()} tokens` : "Context"}
        </button>
        <button
          className="inspect-btn"
          onClick={() => {
            setDraftScope(
              systemScope === "global"
                ? view === "code"
                  ? "global_code"
                  : "global_chat"
                : "local",
            );
            setSystemDraft(
              systemScope === "global"
                ? view === "code"
                  ? globalSystemCode
                  : globalSystemChat
                : systemPrompt,
            );
            setShowSystem(true);
          }}
          title="Instructions (system prompt)"
        >
          <InstructionsIcon />
        </button>
        <select
          className="model-select conn-select"
          value={activeConn}
          onChange={(e) => switchConnection(e.target.value)}
          disabled={busy}
          title="Model source. Keys & base URLs live in Settings → Connections."
        >
          {CONNECTIONS.filter((c) => c.ready).map((c) => (
            <option key={c.id} value={c.id}>
              {c.label}
              {c.needsKey && !keyStatus[c.id] ? " — no key" : ""}
            </option>
          ))}
        </select>
        <select
          className="model-select"
          value={model}
          onChange={(e) => setModel(e.target.value)}
          disabled={busy || models.length === 0}
        >
          {models.length === 0 ? (
            <option value="">no models — check the source?</option>
          ) : (
            models.map((m) => (
              <option key={m} value={m}>
                {m}
              </option>
            ))
          )}
        </select>
        {activeConnDef().reasoning && (
          <select
            className="think-select"
            value={thinkingEffort}
            onChange={(e) => setThinkingEffort(e.target.value)}
            disabled={busy}
            title="Thinking depth — reasoning effort. Off disables it."
          >
            <option value="off">Think · Off</option>
            <option value="low">Think · Low</option>
            <option value="medium">Think · Medium</option>
            <option value="high">Think · High</option>
            <option value="xhigh">Think · X-High</option>
            <option value="max">Think · Max</option>
          </select>
        )}
        <button
          className="model-refresh"
          onClick={refreshModels}
          disabled={busy}
          title="Refresh model list"
          aria-label="Refresh model list"
        >
          <RetryIcon />
        </button>
        {now && <span className="clock">{now}</span>}
        <button
          className="inspect-btn"
          onClick={() => (filesOpen ? setFilesOpen(false) : openFiles())}
          title="Files"
        >
          <FilesIcon />
        </button>
      </header>

      <div
        className="transcript"
        ref={scrollRef}
        onScroll={handleScroll}
        onWheel={(e) => {
          if (e.deltaY < 0) stickRef.current = false;
        }}
      >
        <div className="transcript-inner" ref={contentRef}>
          {messages.length === 0 && !busy && segments.length === 0 && (
            <div className="empty">Start a conversation.</div>
          )}
          {messages.map((m, i) => {
            const isLatest =
              !busy && i === messages.length - 1 && m.role === "assistant";
            const useTimeline =
              m.role === "assistant" && !!m.segments && m.segments.length > 0;
            return (
              <div key={i} className={`msg ${m.role}${isLatest ? " latest" : ""}`}>
                {m.role === "assistant" && !useTimeline && m.reasoning && (
                  <Thinking text={m.reasoning} live={false} />
                )}
                {editingIdx === i ? (
                  <div className="msg-edit">
                    <textarea
                      className="msg-edit-text"
                      value={editDraft}
                      onChange={(e) => setEditDraft(e.target.value)}
                      onKeyDown={(e) => {
                        const inPlace = m.role === "assistant";
                        if (
                          e.key === "Enter" &&
                          (inPlace ? e.metaKey || e.ctrlKey : !e.shiftKey)
                        ) {
                          e.preventDefault();
                          saveEdit(i);
                        } else if (e.key === "Escape") {
                          e.preventDefault();
                          cancelEdit();
                        }
                      }}
                      autoFocus
                      rows={Math.min(24, Math.max(2, editDraft.split("\n").length))}
                    />
                    <div className="msg-edit-actions">
                      <button className="msg-edit-cancel" onClick={cancelEdit}>
                        Cancel
                      </button>
                      <button
                        className="msg-edit-save"
                        onClick={() => saveEdit(i)}
                        disabled={m.role !== "assistant" && !editDraft.trim()}
                      >
                        {m.role === "assistant" ? "Save" : "Save & rerun"}
                      </button>
                    </div>
                  </div>
                ) : (
                  <>
                    {useTimeline ? (
                      <TurnTimeline segments={m.segments!} />
                    ) : (
                      <>
                        <div className={`bubble ${m.role}`}>
                          {m.role === "user" &&
                            m.images &&
                            m.images.length > 0 && (
                              <div
                                style={{
                                  display: "flex",
                                  gap: 8,
                                  flexWrap: "wrap",
                                  marginBottom: m.content ? 8 : 0,
                                }}
                              >
                                {m.images.map((src, j) => (
                                  <img
                                    key={j}
                                    src={src}
                                    alt=""
                                    style={{
                                      maxWidth: 220,
                                      maxHeight: 220,
                                      borderRadius: 8,
                                      display: "block",
                                    }}
                                  />
                                ))}
                              </div>
                            )}
                          {m.role === "assistant" ? (
                            <Markdown text={m.content} />
                          ) : (
                            <>
                              {m.action && (
                                <div
                                  className="action-tag"
                                  title={m.action.description || `Action: ${m.action.name}`}
                                >
                                  <span className="action-tag-icon">⚡</span>
                                  <span className="action-tag-name">{m.action.name}</span>
                                </div>
                              )}
                              <div className="user-text">{m.content}</div>
                            </>
                          )}
                        </div>
                        {m.role === "assistant" &&
                          m.sources &&
                          m.sources.length > 0 &&
                          (() => {
                            const runs = m.sources.filter(
                              (s) => s.kind === "code",
                            );
                            const refs = m.sources.filter(
                              (s) => s.kind !== "code",
                            );
                            return (
                              <>
                                {runs.map((s, j) => (
                                  <CodeCell
                                    key={`code-${j}`}
                                    code={s.text}
                                    output={s.detail ?? ""}
                                  />
                                ))}
                                {refs.length > 0 && (
                                  <Sources items={refs} onOpenNote={openNote} />
                                )}
                              </>
                            );
                          })()}
                      </>
                    )}
                    <div className="msg-meta">
                      {m.paused && (
                        <span
                          className="paused-tag"
                          title="Paused mid-turn — reply continues after your next message"
                        >
                          ⏸ paused
                        </span>
                      )}
                      <div className="msg-actions">
                        <button
                          onClick={() => copyMessage(m.content, i)}
                          title="Copy"
                          aria-label="Copy"
                        >
                          {copiedIdx === i ? <CheckIcon /> : <CopyIcon />}
                        </button>
                        {m.role === "user" && (
                          <button
                            onClick={() => startEdit(i)}
                            title="Edit"
                            aria-label="Edit"
                            disabled={busy}
                          >
                            <EditIcon />
                          </button>
                        )}
                        {m.role === "assistant" && editMessages && (
                          <button
                            onClick={() => startEdit(i)}
                            title="Edit in place (no rerun)"
                            aria-label="Edit in place"
                            disabled={busy}
                          >
                            <EditIcon />
                          </button>
                        )}
                        {isLatest && (
                          <button
                            onClick={regenerate}
                            title="Retry"
                            aria-label="Retry"
                          >
                            <RetryIcon />
                          </button>
                        )}
                        {m.role === "assistant" && (
                          <button
                            onClick={() => openExportMessage(m.content, i)}
                            title="Export"
                            aria-label="Export"
                          >
                            <DownloadIcon />
                          </button>
                        )}
                        <button
                          onClick={() => deleteMessage(i)}
                          title="Delete"
                          aria-label="Delete"
                          disabled={busy}
                        >
                          <TrashIcon />
                        </button>
                      </div>
                      {m.created_at && (
                        <span className="msg-time">
                          {formatTime(m.created_at)}
                        </span>
                      )}
                    </div>
                  </>
                )}
              </div>
            );
          })}
          {busy && (segments.length > 0 || pendingTool || pendingEdit) && (
            <div className="msg assistant latest">
              <TurnTimeline
                segments={segments}
                live={!pendingTool && !pendingEdit}
                pendingEdit={pendingEdit}
                onAnswerEdit={onAnswerEdit}
              />
              {pendingTool && !pendingEdit && (
                <div className="working">{toolLabel(pendingTool)}</div>
              )}
            </div>
          )}
          {(busy || isCompacting) &&
            segments.length === 0 &&
            !pendingTool &&
            !pendingEdit &&
            (isCompacting || messages[messages.length - 1]?.role !== "assistant") && (
              <div className="working">
                {isCompacting ? "compacting conversation…" : "working…"}
              </div>
            )}
          {indexingStatus && (
            <div
              className={`indexing-status-line ${
                indexingStatus.done ? "indexing-done" : "working"
              }`}
            >
              <span>{indexingStatus.message}</span>
              {indexingStatus.active ? (
                <button
                  type="button"
                  className="inline-action-btn"
                  onClick={onStopIndexing}
                  title="Stop indexing"
                >
                  Stop
                </button>
              ) : (
                <button
                  type="button"
                  className="inline-close-btn"
                  onClick={onDismissIndexing}
                  title="Dismiss"
                >
                  ✕
                </button>
              )}
            </div>
          )}
        </div>
      </div>

      {error && (
        <div className="error">
          <span className="error-message">
            {error}
            <button
              type="button"
              className="error-copy-btn"
              onClick={() => {
                navigator.clipboard.writeText(error).catch(() => {});
                setCopiedError(true);
                setTimeout(() => setCopiedError(false), 2000);
              }}
              title="Copy error message to clipboard"
            >
              {copiedError ? <CheckIcon /> : <CopyIcon />}
              <span>{copiedError ? "Copied" : "Copy"}</span>
            </button>
          </span>
          <button
            type="button"
            className="error-dismiss-btn"
            onClick={() => setError(null)}
            aria-label="Dismiss error"
            title="Dismiss"
          >
            ✕
          </button>
        </div>
      )}
      {notice && (
        <div className="notice">
          <span>{notice}</span>
          <button
            type="button"
            className="notice-dismiss-btn"
            onClick={() => setNotice(null)}
            aria-label="Dismiss"
            title="Dismiss"
          >
            ✕
          </button>
        </div>
      )}
      {contextPressure && !nudgeDismissed && !busy && (
        <div className="notice">
          <span>
            Context is at ~{preview!.total_tokens.toLocaleString()} of your{" "}
            {capForNudge!.toLocaleString()} token budget. Run{" "}
            <code>/compact</code> to fold the earlier turns into a handover note.
          </span>
          <button
            type="button"
            className="notice-dismiss-btn"
            onClick={() => setNudgeDismissed(true)}
            aria-label="Dismiss"
            title="Dismiss"
          >
            ✕
          </button>
        </div>
      )}
      {view === "code" && sessionRoot && folderMissing && (
        <div className="notice notice-warning">
          <span>
            ⚠️ Project folder not found: <code>{sessionRoot}</code>. File tools and commands are paused until relocated.
          </span>
          {onChangeSessionRoot && (
            <button
              type="button"
              className="notice-action-btn"
              onClick={onChangeSessionRoot}
            >
              Relocate folder…
            </button>
          )}
        </div>
      )}

      <Composer
        onSubmit={onSend}
        busy={busy}
        isStopping={isStopping}
        model={model}
        stop={stop}
        images={images}
        onAddImages={addImageFiles}
        onRemoveImage={(i) =>
          setImages((prev) => prev.filter((_, k) => k !== i))
        }
        controls={
          <div className="toggles">
            <button
              type="button"
              className="plus-btn"
              onClick={() => setShowToggles((v) => !v)}
              title="Tools & options"
            >
              +
            </button>
            {showToggles && (
              <>
                <div
                  className="popup-backdrop"
                  onClick={() => setShowToggles(false)}
                />
                <div
                  className="toggles-popup"
                  onMouseDown={(e) => e.stopPropagation()}
                >
                  <button
                    type="button"
                    className="upload-btn"
                    onClick={() => {
                      setShowToggles(false);
                      fileInputRef.current?.click();
                    }}
                  >
                    + Attach file
                  </button>
                  <div className="toggles-divider" />
                  {sessionRoot ? (
                    <>
                      <Switch
                        label="Web"
                        checked={webOn}
                        onChange={(v) => {
                          setWebOn(v);
                          setWebSearchOn(v);
                          setWebFetchOn(v);
                        }}
                      />
                      <Switch
                        label="Vision"
                        checked={visionOn}
                        onChange={setVisionOn}
                      />
                      <Switch
                        label="Godot"
                        checked={godotToolsOn}
                        onChange={setGodotToolsOn}
                      />
                      <Switch
                        label="Browser"
                        checked={browserOn}
                        onChange={setBrowserOn}
                      />
                    </>
                  ) : (
                    <>
                      <Switch
                        label="Tools"
                        checked={toolsOn}
                        onChange={setToolsOn}
                      />
                      <div className="toggles-sub">
                        <Switch
                          label="web-search"
                          checked={webSearchOn}
                          onChange={setWebSearchOn}
                          disabled={!toolsOn}
                        />
                        <Switch
                          label="web-fetch"
                          checked={webFetchOn}
                          onChange={setWebFetchOn}
                          disabled={!toolsOn}
                        />
                        <Switch
                          label="notes"
                          checked={mdWriteOn || notesOn}
                          onChange={(v) => {
                            setMdWriteOn(v);
                            setNotesOn(v);
                          }}
                          disabled={!toolsOn}
                        />
                        <Switch
                          label="html-write"
                          checked={htmlWriteOn}
                          onChange={setHtmlWriteOn}
                          disabled={!toolsOn}
                        />
                        <Switch
                          label="python"
                          checked={codeOn}
                          onChange={setCodeOn}
                          disabled={!toolsOn}
                        />
                      </div>
                    </>
                  )}
                  <div className="toggles-divider" />
                  <Switch
                    label="Sampling"
                    checked={samplingOn}
                    onChange={setSamplingOn}
                  />
                  {samplingOn && (
                    <div className="toggles-sub">
                      <ParamSlider
                        label="temperature"
                        value={temperature}
                        min={0}
                        max={2}
                        step={0.05}
                        onChange={setTemperature}
                      />
                      <ParamSlider
                        label="top-p"
                        value={topP}
                        min={0}
                        max={1}
                        step={0.05}
                        onChange={setTopP}
                      />
                      <div className="param-row">
                        <span className="param-label">max tokens</span>
                        <input
                          className="param-num"
                          type="number"
                          min={1}
                          placeholder="∞"
                          value={maxTokens}
                          onChange={(e) => setMaxTokens(e.target.value)}
                        />
                      </div>
                    </div>
                  )}
                </div>
              </>
            )}
          </div>
        }
      />
    </main>
  );
}
