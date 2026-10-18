// ── surface: shared ──────────────────────────────────
// Unit tests for the pure helpers in utils.ts — titles, labels, fence
// balancing, error sniffing, segment rewrites, prefs round-trip.
// Exports: none (test) · Related: src/utils.ts, src/__tests__/turnAssembly.test.ts

import { describe, it, expect, afterEach } from "vitest";
import {
  deriveTitle,
  slugify,
  toolLabel,
  toolIcon,
  formatTime,
  stripCodeFences,
  balanceFences,
  isToolError,
  toolTitle,
  nodeText,
  withEditedText,
  loadPrefs,
  savePrefs,
} from "../utils";
import type { Message, Segment } from "../lib/provider";

describe("deriveTitle", () => {
  it("uses the first non-empty user message, collapsed to one line", () => {
    const msgs: Message[] = [
      { role: "assistant", content: "ignored" },
      { role: "user", content: "  hello   \n\n world  " },
    ];
    expect(deriveTitle(msgs)).toBe("hello world");
  });

  it("truncates at 42 chars with an ellipsis", () => {
    const long = "x".repeat(50);
    expect(deriveTitle([{ role: "user", content: long }])).toBe(
      `${"x".repeat(42)}…`,
    );
  });

  it("falls back to the attached file name when the prompt has no text", () => {
    const msgs: Message[] = [
      {
        role: "user",
        content: "",
        segments: [
          {
            kind: "tool",
            id: "1",
            name: "project_read",
            title: "read",
            args: JSON.stringify({ path: "src/deep/file.ts" }),
          },
        ],
      },
    ];
    expect(deriveTitle(msgs)).toBe("file.ts");
  });

  it("handles Windows backslash separators and file alias in deriveTitle", () => {
    const msgsWindows: Message[] = [
      {
        role: "user",
        content: "",
        segments: [
          {
            kind: "tool",
            id: "1",
            name: "read_file",
            title: "read",
            args: JSON.stringify({ path: "src\\deep\\windows_file.ts" }),
          },
        ],
      },
    ];
    expect(deriveTitle(msgsWindows)).toBe("windows_file.ts");

    const msgsAlias: Message[] = [
      {
        role: "user",
        content: "",
        segments: [
          {
            kind: "tool",
            id: "2",
            name: "read_note",
            title: "read",
            args: JSON.stringify({ file: "notes\\spec.md" }),
          },
        ],
      },
    ];
    expect(deriveTitle(msgsAlias)).toBe("spec.md");
  });

  it("falls back to 'New chat' when there is nothing to derive from", () => {
    expect(deriveTitle([])).toBe("New chat");
    expect(deriveTitle([{ role: "user", content: "   " }])).toBe("New chat");
  });
});

describe("slugify", () => {
  it("lowercases and dashes non-alphanumerics", () => {
    expect(slugify("Hello, World!")).toBe("hello-world");
  });

  it("caps at 60 chars and trims edge dashes", () => {
    expect(slugify(`-${"a".repeat(70)}-`)).toBe("a".repeat(60));
  });

  it("falls back to 'export' when nothing survives", () => {
    expect(slugify("!!! ---")).toBe("export");
    expect(slugify("")).toBe("export");
  });
});

describe("formatTime", () => {
  it("renders local time as 'Mon D, h:mmam/pm'", () => {
    // Built from local components so the expectation is TZ-independent.
    const ts = new Date(2026, 8, 26, 14, 5).getTime();
    expect(formatTime(ts)).toBe("Sep 26, 2:05pm");
    const midnight = new Date(2026, 0, 1, 0, 7).getTime();
    expect(formatTime(midnight)).toBe("Jan 1, 12:07am");
  });
});

describe("stripCodeFences", () => {
  it("removes fence markers but keeps the body", () => {
    expect(stripCodeFences("line\n```\ncode\n```\nend")).toBe(
      "line\n\ncode\n\nend",
    );
  });

  it("defuses an unclosed fence in reasoning text", () => {
    expect(stripCodeFences("think\n```js\nunclosed")).toBe(
      "think\n\nunclosed",
    );
  });
});

describe("balanceFences", () => {
  it("closes a fence the segment left open and reports the opener", () => {
    const r = balanceFences("```\ncode", null);
    expect(r.processedText).toBe("```\ncode\n```");
    expect(r.outgoingFence).toBe("```");
  });

  it("passes balanced text through untouched", () => {
    const r = balanceFences("```\ncode\n```", null);
    expect(r.processedText).toBe("```\ncode\n```");
    expect(r.outgoingFence).toBeNull();
  });

  it("prepends the incoming opener so the continuation re-opens the fence", () => {
    const r = balanceFences("code\n```", "```js");
    expect(r.processedText).toBe("```js\ncode\n```");
    expect(r.outgoingFence).toBeNull();
  });
});

describe("isToolError", () => {
  it("flags explicit error prefixes", () => {
    expect(isToolError("error: no such file")).toBe(true);
    expect(isToolError("Error launching command: boom")).toBe(true);
    expect(isToolError("page read error: 404")).toBe(true);
  });

  it("flags user rejections, timeouts and tracebacks", () => {
    expect(isToolError("The user rejected the edit to 'a'")).toBe(true);
    expect(isToolError("Command timed out after 120s")).toBe(true);
    expect(isToolError("Traceback (most recent call last):\n  ...")).toBe(true);
  });

  it("reads exit codes anywhere in the output", () => {
    expect(isToolError("$ check\nexit code: 1")).toBe(true);
    expect(isToolError("$ check\nexit code: 0")).toBe(false);
    expect(isToolError("exit code: 2\n")).toBe(true);
  });

  it("does not false-positive on prose that merely mentions errors", () => {
    expect(isToolError("the file contains an error message")).toBe(false);
    expect(isToolError("")).toBe(false);
    expect(isToolError(undefined)).toBe(false);
  });

  it("does not false-positive on source code or diffs containing tracebacks or exit codes for content read tools", () => {
    const fileWithTraceback = '1\t// utils.ts\n2\tif (output.includes("Traceback (most recent call last):")) {\n3\t  return true;\n4\t}';
    expect(isToolError(fileWithTraceback, "read_file")).toBe(false);
    expect(isToolError(fileWithTraceback, "project_read")).toBe(false);
    expect(isToolError(fileWithTraceback)).toBe(false); // detects line numbers

    const diffWithExitCode = '=== test.py ===\n@@ -1,2 +1,2 @@\n- exit code: 0\n+ exit code: 1';
    expect(isToolError(diffWithExitCode, "get_diff")).toBe(false);
    expect(isToolError(diffWithExitCode, "project_changes")).toBe(false);

    const searchWithTraceback = 'src/utils.ts (1 hit) [src]\n  >37: if (output.includes("Traceback (most recent call last):"))';
    expect(isToolError(searchWithTraceback, "grep_search")).toBe(false);
    expect(isToolError(searchWithTraceback, "project_search")).toBe(false);

    // But genuine tool errors still flag as errors
    expect(isToolError("error: file 'src/utils.ts' not found", "read_file")).toBe(true);
    expect(isToolError("error: file 'src/utils.ts' not found", "project_read")).toBe(true);
    expect(isToolError("error: grep_search requires a non-empty query", "grep_search")).toBe(true);
    expect(isToolError("error: project_search requires a non-empty query", "project_search")).toBe(true);
    expect(isToolError("The user rejected the file read", "read_file")).toBe(true);
    expect(isToolError("The user rejected the file read", "project_read")).toBe(true);
  });
});

describe("toolTitle", () => {
  it("builds the label from name + args", () => {
    expect(toolTitle("grep_search", '{"query":"foo"}')).toBe("search: foo");
    expect(toolTitle("project_search", '{"query":"foo"}')).toBe("search: foo");
    expect(
      toolTitle("edit_file", '{"path":"src/main.rs","symbol":"start"}'),
    ).toBe("edit: src/main.rs (start)");
    expect(
      toolTitle("project_edit", '{"path":"src/main.rs","symbol":"start"}'),
    ).toBe("edit: src/main.rs (start)");
    expect(
      toolTitle("edit_file", '{"path":"src/main.rs"}'),
    ).toBe("edit: src/main.rs");
    expect(
      toolTitle("project_edit", '{"path":"src/main.rs"}'),
    ).toBe("edit: src/main.rs");
    expect(toolTitle("run_command", '{"argv":["cargo","check"]}')).toBe(
      "run: cargo check",
    );
    expect(toolTitle("run_command", '{"command":"npm run dev","background":true}')).toBe(
      "background: npm run dev",
    );
    expect(toolTitle("process_manage", '{"action":"check","id":1}')).toBe(
      "check #1",
    );
    expect(toolTitle("process_manage", '{"action":"stop","id":1}')).toBe(
      "stop #1",
    );
    expect(toolTitle("process_manage", '{"action":"list"}')).toBe(
      "list processes",
    );
    expect(toolTitle("project_overview", "{}")).toBe("project overview");
    expect(toolTitle("project_info", "{}")).toBe("project info");
    expect(toolTitle("get_diff", "{}")).toBe("diff");
    expect(toolTitle("project_changes", "{}")).toBe("changes this session");
    expect(toolTitle("read_image", '{"path":"ui.png"}')).toBe("read image: ui.png");
    expect(toolTitle("typecheck", "{}")).toBe("typecheck");
    expect(toolTitle("read_file", '{"path":"src/App.tsx"}')).toBe("read: src/App.tsx");
    expect(toolTitle("read_file", '{"path":"src/App.tsx","start":1190,"lines":60}')).toBe("read src/App.tsx (L1190–1249)");
    expect(toolTitle("read_file_lines", '{"path":"src/App.tsx","start":1190,"lines":60}')).toBe("read src/App.tsx (L1190–1249)");
    expect(toolTitle("read_file_lines", '{"path":"src/main.rs","offset":10,"limit":20}')).toBe("read src/main.rs (L10–29)");
    expect(toolTitle("read_file_lines", '{"path":"src/main.rs","start":100,"end":200}')).toBe("read src/main.rs (L100–200)");
    expect(toolTitle("read_file_lines", '{"path":"src/main.rs","start":145,"end":30}')).toBe("read src/main.rs (L145–174)");
    expect(toolTitle("project_read_lines", '{"path":"src/main.rs"}')).toBe("read src/main.rs (L1–end)");
    expect(toolTitle("read_file_lines", "{}")).toBe("read lines");
  });

  it("falls back to the bare name when args are partial mid-stream JSON", () => {
    expect(toolTitle("web_search", '{"query')).toBe("web_search");
    expect(toolTitle("web_search", "")).toBe("web_search");
  });
});

describe("toolLabel / toolIcon", () => {
  it("maps known tools and falls back for unknown ones", () => {
    expect(toolLabel("md_write")).toBe("✎ writing markdown…");
    expect(toolLabel("read_file")).toBe("≡ reading file…");
    expect(toolLabel("edit_file")).toBe("✎ editing file…");
    expect(toolLabel("write_file")).toBe("✎ writing file…");
    expect(toolLabel("project_overview")).toBe("ℹ project overview…");
    expect(toolLabel("typecheck")).toBe("⚙ checking types…");
    expect(toolLabel("grep_search")).toBe("⌕ searching the codebase…");
    expect(toolLabel("project_search")).toBe("⌕ searching the codebase…");
    expect(toolLabel("semantic_search")).toBe("⌖ scanning the codebase…");
    expect(toolLabel("code_search")).toBe("⌖ scanning the codebase…");
    expect(toolLabel("search_code")).toBe("⌖ scanning the codebase…");
    expect(toolLabel("mystery")).toBe("working…");
    expect(toolIcon("read_file")).toBe("≡");
    expect(toolIcon("grep_search")).toBe("⌕");
    expect(toolIcon("project_search")).toBe("⌕");
    expect(toolIcon("semantic_search")).toBe("⌖");
    expect(toolIcon("code_search")).toBe("⌖");
    expect(toolIcon("project_overview")).toBe("ℹ");
    expect(toolIcon("typecheck")).toBe("⚙");
    expect(toolIcon("delete_file")).toBe("␡");
    expect(toolIcon("project_delete")).toBe("␡");
    expect(toolIcon("mystery")).toBe("•");
  });
});

describe("withEditedText", () => {
  it("replaces the first text segment and folds later prose into it", () => {
    const segs: Segment[] = [
      { kind: "reasoning", text: "hmm" },
      { kind: "text", text: "old" },
      { kind: "tool", id: "1", name: "run_command", title: "run" },
      { kind: "text", text: "more" },
    ];
    const out = withEditedText(segs, "new");
    expect(out).toEqual([
      { kind: "reasoning", text: "hmm" },
      { kind: "text", text: "new" },
      { kind: "tool", id: "1", name: "run_command", title: "run" },
    ]);
  });

  it("drops prose entirely when the edit emptied it", () => {
    const segs: Segment[] = [
      { kind: "text", text: "old" },
      { kind: "tool", id: "1", name: "run_command", title: "run" },
    ];
    expect(withEditedText(segs, "")).toEqual([
      { kind: "tool", id: "1", name: "run_command", title: "run" },
    ]);
  });

  it("appends the text when the turn had no prose segment", () => {
    const segs: Segment[] = [
      { kind: "tool", id: "1", name: "run_command", title: "run" },
    ];
    expect(withEditedText(segs, "fresh")).toEqual([
      { kind: "tool", id: "1", name: "run_command", title: "run" },
      { kind: "text", text: "fresh" },
    ]);
  });
});

describe("nodeText", () => {
  it("flattens strings, numbers, arrays and element children", () => {
    const node = {
      props: { children: ["a", 1, { props: { children: "b" } }] },
    };
    expect(nodeText(node)).toBe("a1b");
    expect(nodeText(null)).toBe("");
    expect(nodeText("plain")).toBe("plain");
  });
});

describe("loadPrefs / savePrefs", () => {
  afterEach(() => localStorage.clear());

  it("merges partial saves into the stored prefs and writes outpost_prefs", () => {
    savePrefs({ model: "m1" });
    savePrefs({ theme: "dark" });
    expect(loadPrefs()).toEqual({ model: "m1", theme: "dark" });
    expect(localStorage.getItem("outpost_prefs")).toBeTruthy();
  });

  it("returns empty prefs on corrupt stored JSON", () => {
    localStorage.setItem("outpost_prefs", "not json");
    expect(loadPrefs()).toEqual({});
  });
});
