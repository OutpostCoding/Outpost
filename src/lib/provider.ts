// ── surface: shared ──────────────────────────────────
// Typed wrapper over the Tauri commands + shared types (Segment, Message, ThreadInfo, …).
// Exports: types + API fns · Related: commands.rs, hooks/*

// Thin typed wrapper over the Rust-side commands (spec §2). The webview never
// calls an inference backend directly — it goes through these Tauri commands,
// which run the agent loop and stream normalized events back over a Channel.

import { invoke, Channel } from "@tauri-apps/api/core";

export type Role = "system" | "user" | "assistant" | "tool";

export interface ToolCall {
  id: string;
  name: string;
  arguments: string;
  extra_content?: Record<string, any>;
}

export interface Source {
  kind: string;
  text: string;
  detail?: string;
}

/** One entry in an assistant turn's ordered render timeline. The turn is drawn
 *  as these in the order they streamed — each thought, each bit of prose, and
 *  each tool call as its own inline block — instead of collapsing into one
 *  thinking block + one bubble + a tool pile. UI-only; never sent to the model. */
export type Segment =
  | { kind: "reasoning"; text: string }
  | { kind: "text"; text: string }
  | {
      kind: "tool";
      id: string;
      name: string;
      args?: string;
      /** Compact label, e.g. "web_search: median filter" or "write: renderer.py". */
      title: string;
      /** run_python source — rendered inline as a code cell. */
      code?: string;
      /** The tool's returned result, shown as an expandable snippet. */
      output?: string;
      /** An image the tool produced (data URI) — a screenshot, or a project image
       *  it opened. Shown as a thumbnail so you can see what the model saw; the
       *  model itself receives it via a separate synthetic user turn. */
      image?: string;
      /** Provider-specific metadata (e.g. Gemini thought_signature). */
      extra_content?: Record<string, any>;
    };

export interface Message {
  role: Role;
  content: string;
  /** Image attachments (base64 data URIs) on a user turn, for vision models.
   *  Sent to OpenAI-compatible backends as image_url content parts. */
  images?: string[];
  tool_calls?: ToolCall[];
  tool_call_id?: string;
  created_at?: number;
  reasoning?: string;
  sources?: Source[];
  /** Ordered render timeline for an assistant turn (see `Segment`). Present on
   *  turns produced after the timeline landed; older turns fall back to
   *  `reasoning` + `content` + `sources`. */
  segments?: Segment[];
  /** Pause & reply: this assistant message is a PAUSED partial — committed like a
   *  stop, while the backend parks the bubble for a later resume. UI marker only
   *  (the ⏸ tag), session-scoped: the backend ignores unknown fields on save, so
   *  the tag does not survive a thread reload (like `reasoning_signature`).
   *  Never sent to the model — the wire adapters read content/tool_calls. */
  paused?: boolean;
  /** Action metadata if this turn was initiated via a custom prompt action (e.g. /research). */
  action?: { name: string; description?: string; template?: string };
}

export interface Tool {
  name: string;
  description: string;
  parameters: unknown; // JSON Schema
}

export interface ChatRequest {
  model: string;
  messages: Message[];
  tools?: Tool[];
  temperature?: number | null;
  top_p?: number | null;
  max_tokens?: number | null;
  /** Thinking control level: "off" | "low" | "medium" | "high" | "xhigh" | "max".
   *  How it maps to wire params depends on `reasoning_style`. */
  thinking_effort?: string | null;
  /** Which provider's reasoning dialect to map `thinking_effort` onto: "anthropic"
   *  (native adapter) or "deepseek" (OpenAI adapter → thinking:{type} +
   *  reasoning_effort; GLM shares it). Null/absent = don't send reasoning params
   *  (generic OpenAI servers bake thinking into `<think>`). */
  reasoning_style?: string | null;
  /** Cap on the estimated context (tokens) sent to the model, to stay under a
   *  long-context pricing tier. Null/0 = no cap. */
  max_context_tokens?: number | null;
  /** How many tool-call rounds one turn may take before the loop forces a
   *  wrap-up. Null = the backend default (40). The backend clamps to 1..=200,
   *  so there is no "no cap" — it's a runaway backstop, not a budget. */
  max_tool_calls?: number | null;
}

export type StreamEvent =
  | { type: "delta"; text: string }
  | { type: "reasoning"; text: string }
  | { type: "promote_to_reasoning" }
  | { type: "tool_start"; name: string }
  | { type: "tool_call"; id: string; name: string; arguments: string; extra_content?: Record<string, any> }
  | { type: "tool_result"; id: string; name: string; result: string }
  | { type: "tool_image"; id: string; image: string }
  | { type: "run_code"; id: number; code: string }
  | {
      type: "tool_approval";
      id: number;
      tool: string;
      path: string;
      diff: string;
      warning?: string | null;
      /** run_command only: the command awaiting approval, with the whitelist
       *  patterns an "Always allow" click would persist. `remember` is empty when
       *  the command can never auto-approve (it fails a safety guard) — the card
       *  then shows a disabled "Always allow" instead of one that no-ops. */
      command?: { text: string; remember: string[] } | null;
    }
  | { type: "done"; finish_reason: string | null }
  | { type: "error"; message: string };

/** Which backend a turn routes to: an OpenAI-compatible host or Anthropic, by
 *  base URL + the keychain id under which its key is stored. */
export interface ConnRoute {
  kind: "openai" | "anthropic" | "gemini";
  baseUrl: string;
  keyId: string;
  /** OBSOLETE (2026-08-30): passed through for contract compatibility — the
   *  tail folds into the leading system block on every adapter now, so the
   *  backend no longer consults this. See NOTES.md "single system block". */
  midSystemMessages: boolean;
}

/** List the models a connection serves (also confirms it's reachable). With no
 *  connection, the backend falls back to the default local LM Studio. */
export function listModels(conn?: ConnRoute): Promise<string[]> {
  return invoke<string[]>(
    "list_models",
    conn
      ? {
          providerKind: conn.kind,
          baseUrl: conn.baseUrl,
          keyId: conn.keyId,
          midSystemTail: conn.midSystemMessages,
        }
      : {},
  );
}

/**
 * Run a turn through the agent loop. `onEvent` fires for every normalized event;
 * the returned promise resolves when the whole turn (incl. any tool iterations
 * and approvals) is complete.
 */
export async function sendChat(
  request: ChatRequest,
  enabledTools: string[],
  contextFiles: string[],
  systemPrompt: string,
  profile: string,
  conn: ConnRoute,
  projectRoot: string | null,
  /** The session's Vision switch — gates the native image tools in
   *  enabledToolList(). Sent for contract stability only; no backend gate
   *  reads it (MCP removal, 0.2.17). */
  visionOn: boolean,
  onEvent: (event: StreamEvent) => void,
  readPruneTurns?: number | null,
): Promise<void> {
  const channel = new Channel<StreamEvent>();
  channel.onmessage = onEvent;
  await invoke("send_chat", {
    request,
    enabledTools,
    contextFiles,
    systemPrompt,
    profile,
    providerKind: conn.kind,
    baseUrl: conn.baseUrl,
    keyId: conn.keyId,
    midSystemTail: conn.midSystemMessages,
    projectRoot,
    visionOn,
    readPruneTurns: readPruneTurns ?? null,
    onEvent: channel,
  });
}


/** Compaction state for a thread: a handover note standing in for the first
 *  `up_to` messages. The transcript itself is never modified — this only changes
 *  what gets SENT, so the user keeps full scrollback. */
export interface Compaction {
  summary: string;
  up_to: number;
  /** When it was made, for the UI readout. */
  at: number;
}

/** Summarise `messages` into a handover note for compaction. Nothing is deleted;
 *  the caller stores the note and substitutes it at send time. `budgetChars` is
 *  `null` when no context cap is set — no cap means no clip for summarising. */
export function compactThread(
  messages: Message[],
  priorSummary: string | null,
  budgetChars: number | null,
  model: string,
  conn: ConnRoute,
): Promise<{ summary: string; dropped: number }> {
  return invoke<{ summary: string; dropped: number }>("compact_thread", {
    messages,
    priorSummary,
    budget: budgetChars,
    model,
    providerKind: conn.kind,
    baseUrl: conn.baseUrl,
    keyId: conn.keyId,
  });
}






/** Answer a pending tool approval (e.g. an MD-write diff). */
/** Answer a pending approval: "approve" | "reject" | "rename". */
export function resolveEdit(id: number, decision: string): Promise<void> {
  return invoke("resolve_edit", { id, decision });
}

/** Deliver a sandboxed Python run's captured output back to the waiting tool. */
export function resolveCode(id: number, output: string): Promise<void> {
  return invoke("resolve_code", { id, output });
}

// --- Connections / API keys (stored in the OS keychain, backend-side only) ---

/** Store a connection's API key in the OS keychain. The key never returns. */
export function setApiKey(id: string, key: string): Promise<void> {
  return invoke("set_api_key", { id, key });
}

/** Whether a connection has a key stored — the only key fact the UI learns. */
export function hasApiKey(id: string): Promise<boolean> {
  return invoke<boolean>("has_api_key", { id });
}

/** Remove a connection's stored key. */
export function deleteApiKey(id: string): Promise<void> {
  return invoke("delete_api_key", { id });
}

// --- Persistence (spec §8/§9) ------------------------------------------------

export interface ThreadInfo {
  id: number;
  title: string;
  updated_at: number;
  /** The project folder a coding session is bound to; absent for chat threads. */
  project_root?: string | null;
}

export function listThreads(): Promise<ThreadInfo[]> {
  return invoke<ThreadInfo[]>("list_threads");
}

/** The auto-approval + behaviour policy (mirrors the Rust `PermData`). */
export interface Permissions {
  auto_project_writes: boolean;
  auto_commands: boolean;
  command_whitelist: string[];
}

/** Read the current policy. */
export function getPermissions(): Promise<Permissions> {
  return invoke<Permissions>("get_permissions");
}

/** Replace the policy. */
export function setPermissions(
  autoProjectWrites: boolean,
  autoCommands: boolean,
  commandWhitelist: string[],
): Promise<void> {
  return invoke("set_permissions", {
    autoProjectWrites,
    autoCommands,
    commandWhitelist,
  });
}


/** Stop a managed background process by its id. */
export function stopManagedProcess(id: number): Promise<string> {
  return invoke<string>("stop_managed_process", { id });
}

/** Check if a directory exists on disk. */
export function checkDirectoryExists(path: string): Promise<boolean> {
  return invoke<boolean>("check_directory_exists", { path });
}

/** Batch check whether directories exist on disk. */
export function checkDirectoriesExist(paths: string[]): Promise<boolean[]> {
  return invoke<boolean[]>("check_directories_exist", { paths });
}

/** List all files in a project (POSIX-relative paths) for the code file tree. */
export function listProjectFiles(projectRoot: string): Promise<string[]> {
  return invoke<string[]>("list_project_files", { projectRoot });
}

/** Read one project file (sandboxed to the project root) for the in-drawer code viewer. */
export function readProjectFile(
  projectRoot: string,
  path: string,
): Promise<string> {
  return invoke<string>("read_project_file", { projectRoot, path });
}

/** List relative paths of files modified, added, or deleted in the project this session. */
export function getSessionModifiedFiles(projectRoot: string): Promise<string[]> {
  return invoke<string[]>("get_session_modified_files", { projectRoot });
}

/** Get unified diff of a specific project file against its pre-session snapshot. */
export function getFileDiff(
  projectRoot: string,
  path: string,
): Promise<string | null> {
  return invoke<string | null>("get_file_diff", { projectRoot, path });
}

/** List the markdown docs in a project (POSIX-relative paths) for the viewer. */
export function listProjectDocs(projectRoot: string): Promise<string[]> {
  return invoke<string[]>("list_project_docs", { projectRoot });
}

/** Read one project markdown doc (sandboxed to the project root). */
export function readProjectDoc(
  projectRoot: string,
  path: string,
): Promise<string> {
  return invoke<string>("read_project_doc", { projectRoot, path });
}

/** Write one project markdown doc from the viewer (a direct human edit). */
export function writeProjectDoc(
  projectRoot: string,
  path: string,
  content: string,
): Promise<void> {
  return invoke("write_project_doc", { projectRoot, path, content });
}

export function warmSearchIndex(projectRoot: string): Promise<void> {
  return invoke("warm_search_index", { projectRoot });
}

export interface IndexProgress {
  status: "starting" | "indexing" | "done" | "up_to_date" | "cancelled" | "error";
  current: number;
  total: number;
  message?: string | null;
}

export function startRepoIndexing(
  projectRoot: string,
  onProgress: (progress: IndexProgress) => void,
): Promise<void> {
  const channel = new Channel<IndexProgress>();
  channel.onmessage = onProgress;
  return invoke("start_repo_indexing", { projectRoot, onProgress: channel });
}

export function stopRepoIndexing(): Promise<boolean> {
  return invoke<boolean>("stop_repo_indexing");
}

export function createThread(): Promise<number> {
  return invoke<number>("create_thread");
}

export function loadThread(threadId: number): Promise<Message[]> {
  return invoke<Message[]>("load_thread", { threadId });
}

export function saveThread(
  threadId: number,
  title: string,
  messages: Message[],
): Promise<void> {
  return invoke("save_thread", { threadId, title, messages });
}

/** Duplicate-not-fork: copy the first `upTo` messages into a new thread. */
export function duplicateThread(threadId: number, upTo: number): Promise<number> {
  return invoke<number>("duplicate_thread", { threadId, upTo });
}

export function deleteThread(threadId: number): Promise<void> {
  return invoke("delete_thread", { threadId });
}

export function renameThread(threadId: number, title: string): Promise<void> {
  return invoke("rename_thread", { threadId, title });
}

/** Threads in the trash (soft-deleted), most-recently-deleted first. */
export function listTrashed(): Promise<ThreadInfo[]> {
  return invoke<ThreadInfo[]>("list_trashed");
}

/** Restore a trashed thread to the active list. */
export function restoreThread(threadId: number): Promise<void> {
  return invoke("restore_thread", { threadId });
}

/** Permanently delete one trashed thread (irreversible). */
export function purgeThread(threadId: number): Promise<void> {
  return invoke("purge_thread", { threadId });
}

/** Permanently empty the trash (irreversible). */
export function purgeTrashed(): Promise<void> {
  return invoke("purge_trashed");
}

/** Current date/time in the configured timezone, for the UI readout (§10e). */
export function currentDatetime(): Promise<string> {
  return invoke<string>("current_datetime");
}

/**
 * Interrupt the running turn (Stop, §10). Resolves `true` when a live turn's
 * token was actually cancelled, `false` when there was nothing in flight to
 * stop — the UI only latches its "Stopping…" state on `true`.
 */
export function stopTurn(): Promise<boolean> {
  return invoke<boolean>("stop_turn");
}

// --- Context inspector (spec §10) --------------------------------------------

export interface ContextSection {
  label: string;
  tokens: number;
  content: string;
}

export interface ContextPreview {
  sections: ContextSection[];
  total_tokens: number;
}

/** The exact assembled context for the next turn, with per-section token estimates. */
export function previewContext(
  messages: Message[],
  enabledTools: string[],
  contextFiles: string[],
  systemPrompt: string,
  profile: string,
  projectRoot: string | null,
  visionOn: boolean,
  model: string | null,
  connKind: string | null,
  connBaseUrl: string | null,
  maxContextTokens: number | null,
  readPruneTurns?: number | null,
): Promise<ContextPreview> {
  return invoke<ContextPreview>("preview_context", {
    messages,
    enabledTools,
    contextFiles,
    systemPrompt,
    profile,
    projectRoot,
    visionOn,
    model,
    connKind,
    connBaseUrl,
    maxContextTokens,
    readPruneTurns: readPruneTurns ?? null,
  });
}

/** Export assembled markdown to the notes folder; returns the saved file path. */
export function exportMarkdown(name: string, content: string): Promise<string> {
  return invoke<string>("export_markdown", { name, content });
}

/**
 * Extract an uploaded `.docx` / `.pdf` (raw bytes) to markdown/plain text via the
 * Rust backend, so it can be saved and read like any other note upload.
 */
export function extractDocument(filename: string, bytes: number[]): Promise<string> {
  return invoke<string>("extract_document", { filename, bytes });
}

/**
 * Generate a themed .docx from markdown and save it via a native Save dialog
 * (the user names + places the file), then open it. Resolves to the saved path,
 * or null if the user cancelled. Deterministic — no model involved.
 */
export function exportDocx(markdown: string, defaultName: string): Promise<string | null> {
  return invoke<string | null>("export_docx", { markdown, defaultName });
}

export interface NoteInfo {
  name: string;
  modified: number;
}

/** List the markdown files in the notes folder (newest first). */
export function listNotes(): Promise<NoteInfo[]> {
  return invoke<NoteInfo[]>("list_notes");
}

/** List the HTML pages in the notes folder (newest first). */
export function listPages(): Promise<NoteInfo[]> {
  return invoke<NoteInfo[]>("list_pages");
}

/** Open a saved HTML page in the OS default browser. */
export function openPage(name: string): Promise<void> {
  return invoke("open_page", { name });
}

/** Read one markdown note by its relative path. */
export function readNote(name: string): Promise<string> {
  return invoke<string>("read_note", { name });
}

/** Delete a note from the notes folder. */
export function deleteNote(name: string): Promise<void> {
  return invoke("delete_note", { name });
}

/** Reveal a note in the OS file manager. */
export function revealNote(name: string): Promise<void> {
  return invoke("reveal_note", { name });
}

/** Reveal a folder in the OS file manager. */
export function revealFolder(path: string): Promise<void> {
  return invoke("reveal_folder", { path });
}


/** Rename a note; returns the final name (with .md). */
export function renameNote(oldName: string, newName: string): Promise<string> {
  return invoke<string>("rename_note", { old: oldName, new: newName });
}

/** Load a thread's per-chat config blob (JSON string). */
export function loadThreadConfig(threadId: number): Promise<string> {
  return invoke<string>("load_thread_config", { threadId });
}

/** Persist a thread's per-chat config blob (JSON string). */
export function saveThreadConfig(
  threadId: number,
  config: string,
): Promise<void> {
  return invoke("save_thread_config", { threadId, config });
}

export interface ActionInfo {
  name: string;
  description: string;
  template: string;
  path: string;
  scope: "global" | "workspace";
}

/** List custom prompt actions (global + workspace). */
export function listActions(
  sessionRoot?: string | null,
): Promise<ActionInfo[]> {
  return invoke<ActionInfo[]>("list_actions", {
    sessionRoot: sessionRoot ?? null,
  });
}

/** Reveal the global actions directory in the OS file manager. */
export function revealActionsDir(): Promise<void> {
  return invoke("reveal_actions_dir");
}

export interface EmbeddingModelStatus {
  model_name: string;
  status: "ready" | "not_downloaded" | "downloading" | "error";
  size_bytes: number;
  size_formatted: string;
  cache_dir: string;
  is_loaded: boolean;
  message?: string | null;
}

/** Check readiness, disk size, and loaded state of the local embedding model. */
export function getEmbeddingModelStatus(): Promise<EmbeddingModelStatus> {
  return invoke<EmbeddingModelStatus>("get_embedding_model_status");
}

/** Download and initialize the local embedding model on demand. */
export function downloadEmbeddingModel(): Promise<EmbeddingModelStatus> {
  return invoke<EmbeddingModelStatus>("download_embedding_model");
}

/** Delete the local embedding model from disk to reclaim space. */
export function deleteEmbeddingModel(): Promise<EmbeddingModelStatus> {
  return invoke<EmbeddingModelStatus>("delete_embedding_model");
}

/** Reveal the models directory in the OS file manager. */
export function revealModelsDir(): Promise<void> {
  return invoke("reveal_models_dir");
}


