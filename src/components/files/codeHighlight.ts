// ── surface: files ────────────────────────────────────
// Syntax highlighting helper for in-drawer code viewer.
// Exports: highlightCode, getLanguageForPath, splitHighlightedHtml, escapeHtml
// Related: FilesDrawer.tsx

import hljs from "highlight.js";

// Register Godot Engine GDScript grammar
if (!hljs.getLanguage("gdscript")) {
  hljs.registerLanguage("gdscript", (hljs) => ({
    name: "GDScript",
    aliases: ["gd"],
    keywords: {
      keyword:
        "and in not or self void as assert breakpoint class class_name " +
        "extends is func setget signal tool yield const enum export " +
        "onready static var break continue if elif else for pass return " +
        "match while await super",
      built_in:
        "Color Vector2 Vector2i Vector3 Vector3i Vector4 Vector4i Transform2D Transform3D " +
        "Plane Quaternion Basis AABB Node Node2D Node3D CharacterBody2D CharacterBody3D " +
        "Sprite2D Sprite3D CanvasItem Control Area2D Area3D RayCast2D RayCast3D " +
        "AudioStreamPlayer Camera2D Camera3D PackedByteArray PackedInt32Array PackedStringArray " +
        "Array Dictionary Callable Signal RID Object Resource RefCounted " +
        "int float bool String StringName print printerr push_error push_warning " +
        "randf randf_range randi randi_range deg_to_rad rad_to_deg lerp clamp max min abs " +
        "floor ceil round snapped pow sqrt sin cos tan load preload",
      literal: "true false null PI TAU INF NAN",
    },
    contains: [
      hljs.NUMBER_MODE,
      hljs.HASH_COMMENT_MODE,
      hljs.QUOTE_STRING_MODE,
      hljs.APOS_STRING_MODE,
      {
        className: "meta",
        begin: /@[A-Za-z_]+/,
      },
    ],
  }));
}

// Common file extensions mapped to highlight.js language identifiers
const EXT_MAP: Record<string, string> = {
  rs: "rust",
  ts: "typescript",
  tsx: "typescript",
  js: "javascript",
  jsx: "javascript",
  mjs: "javascript",
  cjs: "javascript",
  py: "python",
  pyw: "python",
  rb: "ruby",
  sh: "bash",
  bash: "bash",
  zsh: "bash",
  ps1: "powershell",
  psm1: "powershell",
  yml: "yaml",
  yaml: "yaml",
  toml: "toml",
  ini: "ini",
  json: "json",
  jsonc: "json",
  md: "markdown",
  markdown: "markdown",
  html: "xml",
  htm: "xml",
  xml: "xml",
  svg: "xml",
  css: "css",
  scss: "scss",
  less: "less",
  sql: "sql",
  go: "go",
  c: "c",
  h: "c",
  cpp: "cpp",
  hpp: "cpp",
  cc: "cpp",
  cxx: "cpp",
  cs: "csharp",
  java: "java",
  kt: "kotlin",
  kts: "kotlin",
  swift: "swift",
  php: "php",
  lua: "lua",
  r: "r",
  dart: "dart",
  diff: "diff",
  patch: "diff",
  graphql: "graphql",
  gql: "graphql",
  proto: "protobuf",
  tex: "latex",
  zig: "zig",

  // Godot Engine
  gd: "gdscript",
  tscn: "ini",
  tres: "ini",
  godot: "ini",
  gdshader: "glsl",
  shader: "glsl",

  // Config & dotfiles
  gitignore: "ini",
  gitattributes: "ini",
  gitmodules: "ini",
  env: "ini",
  cfg: "ini",
  conf: "ini",
  properties: "ini",
};

const FILENAME_MAP: Record<string, string> = {
  dockerfile: "dockerfile",
  makefile: "makefile",
  gemfile: "ruby",
  cmakelists: "cmake",
  "project.godot": "ini",
  ".gitignore": "ini",
  ".gitattributes": "ini",
  ".gitmodules": "ini",
  ".env": "ini",
  ".env.example": "ini",
  ".env.local": "ini",
  ".dockerignore": "ini",
  ".editorconfig": "ini",
  ".npmrc": "ini",
};

export function getLanguageForPath(filePath: string): string | null {
  const name = filePath.split(/[/\\]/).pop() || "";
  const lowerName = name.toLowerCase();

  if (FILENAME_MAP[lowerName]) {
    const lang = FILENAME_MAP[lowerName];
    if (hljs.getLanguage(lang)) return lang;
  }

  const dotIdx = lowerName.lastIndexOf(".");
  if (dotIdx === -1) return null;
  const ext = lowerName.slice(dotIdx + 1);
  if (!ext) return null;

  const mapped = EXT_MAP[ext] || ext;
  if (hljs.getLanguage(mapped)) return mapped;
  if (hljs.getLanguage(ext)) return ext;
  return null;
}

export function escapeHtml(str: string): string {
  return str
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#039;");
}

export function splitHighlightedHtml(html: string): string[] {
  const lines: string[] = [];
  let currentLine = "";
  const stack: string[] = [];
  const regex = /(<span[^>]*>|<\/span>|\n|[^\n<]+|<)/g;
  let match: RegExpExecArray | null;

  while ((match = regex.exec(html)) !== null) {
    const token = match[0];
    if (token === "\n") {
      let closed = currentLine;
      for (let i = stack.length - 1; i >= 0; i--) {
        closed += "</span>";
      }
      lines.push(closed);
      currentLine = stack.join("");
    } else if (token.startsWith("<span")) {
      stack.push(token);
      currentLine += token;
    } else if (token === "</span>") {
      stack.pop();
      currentLine += token;
    } else {
      currentLine += token;
    }
  }
  lines.push(currentLine);
  return lines;
}

export function highlightCode(
  code: string,
  filePath: string,
): { lines: string[]; language: string | null } {
  if (!code) {
    return { lines: [""], language: null };
  }

  const normalized = code.replace(/\r\n/g, "\n").replace(/\r/g, "\n");
  const language = getLanguageForPath(filePath);

  // Safety cutoff: avoid highlighting huge files (> 300KB) to prevent UI lag
  if (normalized.length > 300_000 || !language) {
    return {
      lines: normalized.split("\n").map(escapeHtml),
      language,
    };
  }

  try {
    const highlighted = hljs.highlight(normalized, {
      language,
      ignoreIllegals: true,
    }).value;
    return {
      lines: splitHighlightedHtml(highlighted),
      language,
    };
  } catch {
    return {
      lines: normalized.split("\n").map(escapeHtml),
      language,
    };
  }
}
