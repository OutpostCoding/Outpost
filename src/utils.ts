// ── surface: shared ──────────────────────────────────
// Pure helpers + static tables + types (no React): deriveTitle, slugify, formatTime,
// CONNECTIONS, SLASH_COMMANDS, CODE_CORE, CODE_GROUPS, Prefs, PendingEdit, …
// Exports: many · Related: App.tsx, hooks/*

// Pure functions, constants, and types extracted from App.tsx — no React, no
// JSX. These are either stateless helpers (deriveTitle, slugify, formatTime…),
// static data tables (CONNECTIONS, SLASH_COMMANDS, CODE_CORE…), or the shared
// type declarations (PendingEdit, Connection, Prefs) that App.tsx and the
// component module both need. Keeping them here means App.tsx is just the App
// component and its handlers.

import type { Message, Segment } from "./lib/provider";

// ── Types ───────────────────────────────────────────────────────────────────

export interface PendingEdit {
  id: number;
  tool: string;
  path: string;
  diff: string;
  warning?: string | null;
  /** For run_command approvals only: the command awaiting approval and the
   *  whitelist patterns an "Always allow" click would persist. `remember` is empty
   *  when the command can never auto-approve (it fails a safety guard), so the
   *  card renders a disabled button. Null/absent for every other tool. */
  command?: { text: string; remember: string[] } | null;
}

// A model source: an OpenAI-compatible endpoint (LM Studio + most hosted APIs,
// one shared adapter) or Anthropic (its own native adapter). Base URLs are
// editable in Settings; keys live in the OS keychain, keyed by id.
// ──> To add/remove a provider, edit the CONNECTIONS array below.
//     Full guide: src/CONNECTIONS.md
export type Connection = {
  id: string;
  label: string;
  kind: "openai" | "anthropic";
  baseUrl: string;
  needsKey: boolean;
  ready: boolean; // false = adapter not wired yet, so selecting it is blocked
  // Which reasoning-control dialect this provider speaks, if any. Set = the Think
  // dropdown shows and the backend maps its level to the right wire params.
  // "anthropic" → thinking/adaptive + output_config.effort (native adapter);
  // "deepseek" → thinking:{type} + reasoning_effort (OpenAI adapter; GLM shares it);
  // "openai" → bare reasoning_effort (OpenAI o-series/gpt-5, and Gemini + Mistral via
  //   their OpenAI-compatible endpoints); "openrouter" → reasoning:{effort} (its
  //   unified param, normalized across the whole catalogue); "inception" → reasoning_summary:true + reasoning_effort.
  reasoning?: "anthropic" | "deepseek" | "openai" | "openrouter" | "inception";
  // OBSOLETE (2026-08-30): the per-turn tail folds into the one leading system
  // block on every adapter, so this capability is no longer consulted anywhere.
  // Kept only so saved connections don't break the contract — retire it with
  // the UI. See NOTES.md "single system block".
  midSystemMessages?: boolean;
};

export type Prefs = {
  theme?: "dark" | "light";
  model?: string;
  toolsOn?: boolean;
  webSearchOn?: boolean;
  webFetchOn?: boolean;
  // Vision (off by default): lets a coding session hand the model actual images
  // via project_read_image. Only useful with a vision-capable model, so it's a
  // switch you flip rather than something Outpost infers per model.
  visionOn?: boolean;
  // Godot scene tools (off by default): lets a coding session read .tscn scene
  // files via godot_scene_tree. Specialised — only game development needs it,
  // and its definition tokens are skipped entirely while off.
  godotToolsOn?: boolean;
  mdWriteOn?: boolean;
  notesOn?: boolean;
  htmlWriteOn?: boolean;
  codeOn?: boolean;
  samplingOn?: boolean;
  temperature?: number;
  topP?: number;
  maxTokens?: string;
  thinkingEffort?: string;
  maxContext?: string;
  /** Tool-call rounds allowed in one turn. Empty = the backend default (40).
   *  Stored as a string because it is a text field; parsed at send time. */
  maxToolCalls?: string;
  /** Global system prompt for the Chat side (split 2026-08-24). */
  system_global_chat?: string;
  /** Global system prompt for the Code side (split 2026-08-24). */
  system_global_code?: string;
  /** Legacy single global system prompt (pre-split); read for migration only. */
  system_global?: string;
  profile_name?: string;
  profile_preferred?: string;
  profile_about?: string;
  profile_share?: boolean;
  active_conn?: string;
  conn_base_urls?: Record<string, string>;
  // Opt-in (off by default): show an in-place Edit control on model responses, so
  // you can scrub a line from a past turn without rerunning it.
  edit_messages?: boolean;
  webOn?: boolean;
  browserOn?: boolean;
  batchEditsOn?: boolean;
  lineEditsOn?: boolean;
  // Lite tool profile (legacy): trims a coding session to the CORE build
  // loop, with the peripheral groups off until switched on.
  liteTools?: boolean;
  // Which optional tool groups are on while liteTools is active. Absent/false = off.
  codeGroups?: Record<string, boolean>;
  // Read stub pruning turns (default 2 turns, Claude Code style; 1 = aggressive, 0/null = unlimited).
  read_prune_turns?: number | null;
  // Enforce sequential tool calls (single tool call per assistant generation).
  // Defaults to false (opt-in) to protect small/local models and prevent LM Studio streaming diff crashes.
  sequentialToolCalls?: boolean;
};

// ── Constants ─────────────────────────────────────────────────────────────────

export const FULL_COPY = 1_000_000_000;

// Approval-overlay lookups: declared once as data so adding a tool is a
// one-line edit, not surgery on a nested ternary / || chain.
export const PREVIEW_AS_TEXT = new Set([
  "run_command",
  "delete_file",
  "project_delete",
  "rename_file",
  "project_rename",
  "process_start",
  "restore_file",
  "project_restore",
]);
export const APPROVE_LABEL: Record<string, string> = {
  run_command: "Run command",
  delete_file: "Approve & delete",
  project_delete: "Approve & delete",
  rename_file: "Approve & rename",
  project_rename: "Approve & rename",
  process_start: "Start process",
  restore_file: "Restore",
  project_restore: "Restore",
};

export const CONNECTIONS: Connection[] = [
  // 1. Local-first (offline & private)
  { id: "lmstudio", label: "Local", kind: "openai", baseUrl: "http://localhost:1234/v1", needsKey: false, ready: true },

  // 2. Universal gateway
  { id: "openrouter", label: "OpenRouter", kind: "openai", baseUrl: "https://openrouter.ai/api/v1", needsKey: true, ready: true, reasoning: "openrouter" },

  // 3. Western frontier labs
  { id: "anthropic", label: "Anthropic — Claude", kind: "anthropic", baseUrl: "https://api.anthropic.com", needsKey: true, ready: true, reasoning: "anthropic", midSystemMessages: true },
  { id: "openai", label: "OpenAI", kind: "openai", baseUrl: "https://api.openai.com/v1", needsKey: true, ready: true, reasoning: "openai" },
  { id: "gemini", label: "Google Gemini", kind: "openai", baseUrl: "https://generativelanguage.googleapis.com/v1beta/openai", needsKey: true, ready: true, reasoning: "openai" },
  { id: "xai", label: "xAI (Grok)", kind: "openai", baseUrl: "https://api.x.ai/v1", needsKey: true, ready: true },

  // 4. European flagship
  { id: "mistral", label: "Mistral", kind: "openai", baseUrl: "https://api.mistral.ai/v1", needsKey: true, ready: true, reasoning: "openai" },

  // 5. Major Asian / open-weights labs
  { id: "deepseek", label: "DeepSeek", kind: "openai", baseUrl: "https://api.deepseek.com/v1", needsKey: true, ready: true, reasoning: "deepseek" },
  { id: "qwen", label: "Qwen (DashScope)", kind: "openai", baseUrl: "https://dashscope-intl.aliyuncs.com/compatible-mode/v1", needsKey: true, ready: true },
  { id: "zai", label: "Z.ai (GLM)", kind: "openai", baseUrl: "https://api.z.ai/api/paas/v4", needsKey: true, ready: true, reasoning: "deepseek" },
  { id: "mimo", label: "Xiaomi MiMo", kind: "anthropic", baseUrl: "https://api.xiaomimimo.com/anthropic", needsKey: true, ready: true },

  // 6. Specialized & high-speed inference
  { id: "inception", label: "Inception AI (Mercury)", kind: "openai", baseUrl: "https://api.inceptionlabs.ai/v1", needsKey: true, ready: true, reasoning: "inception" },
  { id: "groq", label: "Groq", kind: "openai", baseUrl: "https://api.groq.com/openai/v1", needsKey: true, ready: true, reasoning: "openai" },

  // 7. Universal custom endpoint
  { id: "custom", label: "Custom (OpenAI-compatible)", kind: "openai", baseUrl: "", needsKey: true, ready: true },
];

/** Commands offered by the composer's slash menu. Adding one here is all it takes
 *  for it to be discoverable — the menu is the only place they're advertised. */
export const SLASH_COMMANDS: { name: string; arg: string | null; hint: string }[] = [
  {
    name: "compact",
    arg: null,
    hint: "Fold the session into one handover note (prior note + new turns)",
  },
  {
    name: "clear",
    arg: null,
    hint: "Clear the transcript but keep the compaction note and index",
  },
  {
    name: "uncompact",
    arg: null,
    hint: "Discard the handover note and send the full transcript again",
  },
  {
    name: "index",
    arg: null,
    hint: "Pre-compute vector search index for the active project in the background",
  },
];

// The coding-session tool set, split into a stable CORE (always present) and
// optional GROUPS. Full profile (liteTools off) = core + every group, which is
// exactly the set the code surface has always offered. Lite profile (for small
// local models, which fumble a long tool list) = core + only the groups switched
// on. Either way the set is decided BEFORE the turn and held for the session, so
// it stays a stable cache prefix — no mid-turn churn.
//
// CORE is the whole build & run loop: orientation, reads, declarative single-file edits
// and writes, safe file management (delete/rename/restore backed by Outpost's automatic
// snapshots), finite command execution, background process/dev-server management
// (servers), and diff inspection.
// All code modifications are routed through edit_file (powered by the multi-tier
// forgiving snippet matcher) and write_file.
export const CODE_CORE = [
  "project_overview",
  "list_dir",
  "code_search",
  "grep_search",
  "find_symbols",
  "typecheck",
  "read_file",
  "read_file_lines",
  "write_file",
  "edit_file",
  "delete_file",
  "rename_file",
  "restore_file",
  "run_command",
  "process_manage",
  "get_diff",
];

// Elective groups for the code surface.
export const CODE_GROUPS: Record<string, string[]> = {
  browser: ["browser_open", "browser_read", "browser_act", "browser_reload", "browser_close"],
};

// ── Per-thread tool switches ────────────────────────────────────────────────
// The thread config's `tools` field: every per-turn tool toggle, stored per
// chat/session (NOT in the global prefs blob). Hydration applies the
// thread's own set on switch (App.tsx), the per-thread save effect persists it,
// and newChat/newSession write the kind defaults at creation. The backend treats
// the config as an opaque JSON blob — no Rust schema.
export type ThreadTools = {
  toolsOn: boolean;
  webSearchOn: boolean;
  webFetchOn: boolean;
  webOn?: boolean;
  visionOn: boolean;
  godotToolsOn: boolean;
  browserOn?: boolean;
  batchEditsOn?: boolean;
  lineEditsOn?: boolean;
  // Legacy fields kept for backward compatibility with previously persisted threads
  liteTools?: boolean;
  codeGroupsOn?: Record<string, boolean>;
  mdWriteOn: boolean;
  notesOn: boolean;
  htmlWriteOn: boolean;
  codeOn: boolean;
  // Sampling: off by default. When on, temperature/top_p are applied and
  // maxTokens (a string; "" = no cap) bounds the reply length.
  samplingOn: boolean;
  temperature: number;
  topP: number;
  maxTokens: string;
};

export const CHAT_TOOLS_DEFAULT: ThreadTools = {
  toolsOn: true,
  webSearchOn: true,
  webFetchOn: true,
  webOn: true,
  visionOn: false,
  godotToolsOn: false,
  browserOn: false,
  liteTools: false,
  codeGroupsOn: {},
  mdWriteOn: false,
  notesOn: false,
  htmlWriteOn: false,
  codeOn: false,
  // Sampling defaults (user decision): off (model keeps its own defaults);
  // when on — temperature 1.0, top-p 0.95, no max-token cap.
  samplingOn: false,
  temperature: 1.0,
  topP: 0.95,
  maxTokens: "",
};

export const SESSION_TOOLS_DEFAULT: ThreadTools = {
  toolsOn: false,
  webSearchOn: false,
  webFetchOn: false,
  webOn: false,
  visionOn: false,
  godotToolsOn: false,
  browserOn: false,
  liteTools: false,
  codeGroupsOn: {},
  mdWriteOn: false,
  notesOn: false,
  htmlWriteOn: false,
  codeOn: false,
  samplingOn: false,
  temperature: 1.0,
  topP: 0.95,
  maxTokens: "",
};

// ── Pure functions ────────────────────────────────────────────────────────────

export function deriveTitle(messages: Message[]): string {
  const firstUserWithText = messages.find(
    (m) => m.role === "user" && m.content.trim(),
  );
  if (firstUserWithText) {
    const t = firstUserWithText.content.trim().replace(/\s+/g, " ");
    return t.length > 42 ? `${t.slice(0, 42)}…` : t;
  }
  // When an attachment was sent with no prompt text, derive from the attached file name.
  for (const m of messages) {
    if (m.segments?.length) {
      for (const s of m.segments) {
        if (
          s.kind === "tool" &&
          (s.name === "read_note" || s.name === "project_read" || s.name === "read_file") &&
          s.args
        ) {
          try {
            const path = (JSON.parse(s.args) as { path?: unknown }).path;
            if (typeof path === "string" && path) {
              const file = path.split("/").pop() ?? path;
              return file.length > 42 ? `${file.slice(0, 42)}…` : file;
            }
          } catch {}
        }
      }
    }
  }
  return "New chat";
}

export function slugify(s: string): string {
  return (
    s
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, "-")
      .replace(/^-+|-+$/g, "")
      .slice(0, 60) || "export"
  );
}

// A grounding label shown while a (buffered) tool call is being generated.
export function toolLabel(name: string): string {
  switch (name) {
    case "md_write":
      return "✎ writing markdown…";
    case "md_edit":
      return "✎ editing markdown…";
    case "web_search":
      return "⌕ searching the web…";
    case "web_fetch":
      return "≡ fetching a page…";
    case "grep_search":
    case "project_search":
      return "⌕ searching the codebase…";
    case "semantic_search":
    case "code_search":
    case "search_code": // legacy name in pre-rename transcripts
      return "⌖ scanning the codebase…";
    case "search_notes":
      return "⌕ searching your notes…";
    case "search_pages":
      return "⌕ searching your pages…";
    case "read_note":
      return "≡ reading a note…";
    case "read_file":
    case "read_file_lines":
    case "project_read":
    case "project_read_lines":
      return "≡ reading file…";
    case "write_file":
    case "project_write":
      return "✎ writing file…";
    case "edit_file":
    case "project_edit":
      return "✎ editing file…";
    case "delete_file":
    case "project_delete":
      return "␡ deleting file…";
    case "rename_file":
    case "project_rename":
      return "↦ renaming file…";
    case "list_dir":
    case "project_list":
      return "▤ listing directory…";
    case "project_overview":
    case "project_info":
      return "ℹ project overview…";
    case "find_symbols":
    case "code_symbols":
      return "λ scanning symbols…";
    case "typecheck":
      return "⚙ checking types…";
    case "get_diff":
    case "project_changes":
      return "± inspecting diff…";
    case "read_image":
    case "project_read_image":
      return "◫ reading image…";
    case "html_write":
      return "✎ writing html…";
    case "html_edit":
      return "✎ editing html…";
    case "run_command":
      return "▶ running command…";
    case "process_manage":
      return "⚙ managing process…";
    case "run_python":
      return "▶ running python…";
    default:
      return "working…";
  }
}

export function formatTime(ts: number): string {
  const d = new Date(ts);
  const date = d.toLocaleDateString("en-US", { month: "short", day: "numeric" });
  let h = d.getHours();
  const min = d.getMinutes().toString().padStart(2, "0");
  const ampm = h >= 12 ? "pm" : "am";
  h = h % 12 || 12;
  return `${date}, ${h}:${min}${ampm}`;
}

// Reasoning is the model's own thinking, not authored prose — and small models
// routinely open a ``` code fence in their thinking and never close it. CommonMark
// then renders everything from that fence to the end as one runaway monospace
// block (looks broken, swallows the rest of the reasoning). Strip the bare fence
// markers so the thinking body flows as normal text; math, bold, and lists still
// render (unlike rendering thinking as flat text). ANSWERS are left untouched —
// their fences are deliberate and pair with syntax highlighting.
export function stripCodeFences(text: string): string {
  return text.replace(/^[ \t]*(?:`{3,}|~{3,}).*$/gm, "");
}

/**
 * Ensures code fences (``` or ~~~) are balanced across separated markdown segments.
 * If incoming text continues an unclosed fence from a previous segment, it prepends the opening fence.
 * If the segment leaves a fence unclosed, it appends a closing fence so formatting does not leak,
 * and returns the open fence info for the subsequent segment.
 */
export function balanceFences(
  text: string,
  incomingFence: string | null
): { processedText: string; outgoingFence: string | null } {
  let processed = text;
  if (incomingFence) {
    processed = incomingFence + "\n" + processed;
  }

  const fenceRegex = /^[ \t]*(`{3,}|~{3,})([^\n]*)$/gm;
  let currentFence: { char: string; len: number; tag: string } | null = null;
  let match: RegExpExecArray | null;

  while ((match = fenceRegex.exec(processed)) !== null) {
    const delimiter = match[1];
    const char = delimiter[0];
    const len = delimiter.length;
    const tag = match[2].trim();

    if (!currentFence) {
      currentFence = { char, len, tag };
    } else if (char === currentFence.char && len >= currentFence.len) {
      currentFence = null;
    }
  }

  if (currentFence) {
    const closeFence = currentFence.char.repeat(currentFence.len);
    const outgoing =
      currentFence.char.repeat(currentFence.len) +
      (currentFence.tag ? " " + currentFence.tag : "");
    return {
      processedText: processed + "\n" + closeFence,
      outgoingFence: outgoing,
    };
  }

  return {
    processedText: processed,
    outgoingFence: null,
  };
}

// Glyph for a tool call in the UI (inline timeline, approval header). Derived from
// the tool name so it stays consistent whether live or reloaded from disk. Clean
// monochrome symbols rather than full-color emojis.
export function toolIcon(name: string): string {
  switch (name) {
    case "web_search":
    case "search_notes":
    case "search_pages":
    case "grep_search":
    case "project_search":
      return "⌕";
    case "semantic_search":
    case "code_search":
    case "search_code": // legacy name in pre-rename transcripts
      return "⌖";
    case "web_fetch":
    case "read_note":
    case "read_file":
    case "read_file_lines":
    case "project_read":
    case "project_read_lines":
      return "≡";
    case "read_image":
    case "project_read_image":
    case "browser_screenshot":
      return "◫";
    case "browser_reload":
      return "↻";
    case "list_dir":
    case "project_list":
      return "▤";
    case "project_overview":
    case "project_info":
      return "ℹ";
    case "find_symbols":
    case "code_symbols":
      return "λ";
    case "godot_scene_tree":
      return "⑂";
    case "run_python":
      return "▶";
    case "write_file":
    case "project_write":
    case "md_write":
    case "html_write":
    case "edit_file":
    case "project_edit":
    case "project_edit_batch":
    case "md_edit":
    case "html_edit":
      return "✎";
    case "run_command":
      return "▶";
    case "delete_file":
    case "project_delete":
      return "␡";
    case "rename_file":
    case "project_rename":
      return "↦";
    case "restore_file":
    case "project_restore":
      return "↩";
    case "get_diff":
    case "project_changes":
      return "±";
    case "typecheck":
    case "process_manage":
      return "⚙";
    case "run_background_process":
    case "process_start":
      return "▶";
    case "process_wait":
      return "◷";
    case "process_logs":
      return "≡";
    case "process_list":
      return "▤";
    case "process_stop":
      return "■";
    case "browser_open":
    case "browser_read":
    case "browser_act":
      return "↗";
    case "browser_close":
      return "✕";
    default:
      return "•";
  }
}

/** Standard monochrome status glyphs for tool chips and turn items. */
export const STATUS_GLYPH = {
  success: "✓",
  error: "✕",
  running: "…",
  rejected: "⊘",
} as const;

/** Check if a tool output represents a genuine tool failure.
 *  Prevents false positives on project_read, project_search, etc., where the source code
 *  or search results legitimately contain the word "error", "exit code: 1", or Python tracebacks.
 */
export function isToolError(output?: string, toolName?: string): boolean {
  if (!output) return false;
  const trimmed = output.trim();
  const lower = trimmed.toLowerCase();

  // Explicit error prefixes
  if (
    lower.startsWith("error:") ||
    lower.startsWith("error ") ||
    lower.startsWith("page read error:") ||
    lower.startsWith("error launching command:")
  ) {
    return true;
  }

  // User rejections and cancellations from backend approval / execution
  if (
    lower.startsWith("the user rejected") ||
    lower.startsWith("the user declined") ||
    lower.startsWith("stopped by the user") ||
    lower.startsWith("could not request approval")
  ) {
    return true;
  }

  // Content-inspection / file-reading / search tools return source text or diffs.
  // Their failures always start with an explicit error prefix above (e.g. "error:").
  // Substrings like "exit code: 1" or "Traceback (most recent call last):" inside the file
  // or diff must NEVER trigger a false failure in the UI.
  const isContentReadTool =
    toolName &&
    [
      "read_file",
      "read_file_lines",
      "grep_search",
      "semantic_search",
      "find_symbols",
      "get_diff",
      "project_overview",
      "project_read",
      "project_read_lines",
      "project_search",
      "code_search",
      "code_symbols",
      "project_changes",
      "file_history",
      "project_info",
    ].includes(toolName);

  if (isContentReadTool) {
    return false;
  }

  // Heuristic when toolName is omitted: line-numbered reads (e.g. "1\t...", "12\t...")
  // or range headers ("'path' lines X–Y:") are source file contents.
  if (/^(?:\d+\t|'[^']+' lines \d+–\d+)/.test(trimmed)) {
    return false;
  }

  // Exit codes anywhere in output (e.g. typecheck "$ check\nexit code: 1")
  const exitMatch = output.match(/(?:^|\n)exit code:\s*(\S+)/);
  if (exitMatch && exitMatch[1] !== "0") {
    return true;
  }

  if (lower.startsWith("command timed out")) {
    return true;
  }

  if (output.includes("Traceback (most recent call last):")) {
    return true;
  }

  return false;
}

// A compact one-line label for a tool call, built from its name + arguments at
// call time (so it survives reloads without re-parsing args). No emoji here —
// the icon is derived separately from the name.
export function toolTitle(name: string, args: string): string {
  let a: Record<string, unknown> = {};
  try {
    a = JSON.parse(args);
  } catch {
    /* args may be partial/non-JSON mid-stream — fall back to the bare name */
  }
  const s = (k: string) => (typeof a[k] === "string" ? (a[k] as string) : "");
  const at = (base: string, v: string) => (v ? `${base}: ${v}` : base);
  switch (name) {
    case "web_search":
      return at("web_search", s("query"));
    case "web_fetch":
      return at("web_fetch", s("url"));
    case "search_notes":
      return at("search_notes", s("query"));
    case "search_pages":
      return at("search_pages", s("query"));
    case "read_note":
      return at("read_note", s("path"));
    case "grep_search":
    case "project_search":
    case "semantic_search":
    case "code_search":
    case "search_code": // legacy name in pre-rename transcripts
      return at("search", s("query"));
    case "read_file":
    case "project_read":
      return at("read", s("path"));
    case "list_dir":
    case "project_list":
      return at("list", s("path") || ".");
    case "project_overview":
      return "project overview";
    case "project_info":
      return "project info";
    case "find_symbols":
    case "code_symbols":
      return at("symbols", s("path"));
    case "typecheck":
      return "typecheck";
    case "godot_scene_tree":
      return at("scene tree", s("scene") || s("path"));
    case "write_file":
    case "project_write":
    case "md_write":
    case "html_write":
      return at("write", s("path"));
    case "edit_file":
    case "project_edit": {
      const p = s("path");
      const sym = s("symbol");
      if (sym && p) {
        return at("edit", `${p} (${sym})`);
      }
      return at("edit", p);
    }
    case "md_edit":
    case "html_edit":
      return at("edit", s("path"));
    case "delete_file":
    case "project_delete":
      return at("delete", s("path"));
    case "rename_file":
    case "project_rename": {
      const f = s("from");
      const t = s("to");
      return f && t ? `rename ${f} → ${t}` : "rename";
    }
    case "run_command": {
      const av = Array.isArray(a["argv"]) ? (a["argv"] as string[]).join(" ") : "";
      const cmd = s("command") || s("cmd") || av;
      if (a["background"] === true) {
        return at("background", cmd);
      }
      return at("run", cmd);
    }
    case "process_manage": {
      const action = s("action") || "check";
      const id = a["id"];
      if (action === "list") return "list processes";
      if (action === "stop") return id ? `stop #${id}` : "stop process";
      return id ? `check #${id}` : "check process";
    }
    case "run_python":
      return "run_python";
    case "read_file_lines":
    case "project_read_lines": {
      const p = s("path");
      const sl = typeof a["start"] === "number" ? a["start"] : "?";
      const el = typeof a["end"] === "number" ? a["end"] : "end";
      return p ? `read ${p} (L${sl}–${el})` : "read lines";
    }
    case "read_image":
    case "project_read_image": {
      const p = s("path");
      return p ? `read image: ${p}` : "read image";
    }
    case "browser_screenshot":
      return "screenshot the page";
    case "browser_reload":
      return "reload the page";
    case "project_edit_batch": {
      const n = Array.isArray(a["edits"]) ? a["edits"].length : "?";
      return `batch edit (${n})`;
    }
    case "restore_file":
    case "project_restore":
      return at("restore", s("path") || "all");
    case "get_diff":
      return "diff";
    case "project_changes":
      return "changes this session";
    case "run_background_process":
    case "process_start": {
      const av = Array.isArray(a["argv"]) ? (a["argv"] as string[]).join(" ") : "";
      return at("start", s("command") || av);
    }
    case "process_wait":
      return `wait #${a["id"] ?? "?"}`;
    case "process_logs":
      return `logs #${a["id"] ?? "?"}`;
    case "process_list":
      return "processes";
    case "process_stop":
      return `stop #${a["id"] ?? "?"}`;
    case "browser_open":
      return at("view", s("url"));
    case "browser_read":
      return "read page";
    case "browser_act":
      return "mouse";
    case "browser_close":
      return "close view";
    default:
      return name;
  }
}

// Flatten a React node tree to its text — used to grab a code block's source for
// the copy button.
export function nodeText(node: any): string {
  if (node == null) return "";
  if (typeof node === "string") return node;
  if (typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(nodeText).join("");
  if (node.props) {
    if (typeof node.props.chart === "string") return node.props.chart;
    return nodeText(node.props.children);
  }
  return "";
}

// Global, app-wide UI preferences (model, sampling, tools) — sticky across
// launches via localStorage, distinct from the per-chat thread config.
export function loadPrefs(): Prefs {
  try {
    const raw = localStorage.getItem("outpost_prefs");
    return raw ? JSON.parse(raw) : {};
  } catch {
    return {};
  }
}

export function savePrefs(prefs: Partial<Prefs>): void {
  try {
    const current = loadPrefs();
    localStorage.setItem("outpost_prefs", JSON.stringify({ ...current, ...prefs }));
  } catch (e) {
    console.error("Failed to save prefs:", e);
  }
}

// Push an in-place edit of an assistant turn back into its timeline segments.
//
// A segmented turn RENDERS from `segments`, not from `content` — so editing content
// alone changes what the model is re-sent while the screen stays identical, which
// reads as "save did nothing" (and leaves the offending line visible). The editor
// shows `content`, i.e. the turn's prose already concatenated, so consolidating the
// display to match what was actually edited is the honest mapping: the edited text
// takes the position of the FIRST text segment and any later text segments fold
// into it. Tool calls and reasoning keep their order and content untouched — the
// record of what the model DID is preserved; only its prose is rewritten.
export function withEditedText(segments: Segment[], text: string): Segment[] {
  const out: Segment[] = [];
  let placed = false;
  for (const seg of segments) {
    if (seg.kind === "text") {
      if (!placed && text) {
        out.push({ kind: "text", text });
        placed = true;
      }
      continue; // later prose folds into the first block (or is dropped if emptied)
    }
    out.push(seg);
  }
  if (!placed && text) out.push({ kind: "text", text });
  return out;
}

// Read an image file and return a JPEG data URI, downscaled to a max edge so the
// base64 stays small — raw photos would bloat the store + the context, and vision
// models want ~1024px anyway.
export async function imageFileToDataUrl(file: File, maxEdge = 1024): Promise<string> {
  try {
    const bitmap = await createImageBitmap(file);
    const scale = Math.min(1, maxEdge / Math.max(bitmap.width, bitmap.height));
    const w = Math.max(1, Math.round(bitmap.width * scale));
    const h = Math.max(1, Math.round(bitmap.height * scale));
    const canvas = document.createElement("canvas");
    canvas.width = w;
    canvas.height = h;
    const ctx = canvas.getContext("2d");
    if (!ctx) return "";
    ctx.drawImage(bitmap, 0, 0, w, h);
    bitmap.close?.();
    return canvas.toDataURL("image/jpeg", 0.85);
  } catch {
    return "";
  }
}

/**
 * Only allow standard web protocols (http, https, mailto) to be handed to the OS
 * shell via openUrl. Refuses dangerous custom schemes (e.g. ms-settings:, file:, cmd:).
 */
export function isSafeExternalUrl(url: string): boolean {
  try {
    const parsed = new URL(url);
    return parsed.protocol === "http:" || parsed.protocol === "https:" || parsed.protocol === "mailto:";
  } catch {
    return false;
  }
}
