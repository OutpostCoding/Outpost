// ── surface: code ──────────────────────────────────
// Turn-assembly tests for useCompactionManager's forModel — the message view
// the model actually sees. Regression home for the 0.3.2 aliasing bug
// (duplicate assistant bubble) and the framing folds (staged files).
// Exports: none (test) · Related: hooks/useCompactionManager.ts, App.tsx runTurn

import { describe, it, expect, vi, afterEach } from "vitest";
import { renderHook, act, cleanup } from "@testing-library/react";
import {
  useCompactionManager,
  type CompactionManagerOptions,
} from "../hooks/useCompactionManager";
import type { Message } from "../lib/provider";
import { compactThread } from "../lib/provider";

// The hook's effect fires previewContext on mount and every dep change; a
// never-settling promise keeps it silent (no act warnings, no state writes).
vi.mock("../lib/provider", () => ({
  compactThread: vi.fn(() => new Promise(() => {})),
  previewContext: vi.fn(() => new Promise(() => {})),
}));

function baseOpts(
  over: Partial<CompactionManagerOptions> = {},
): CompactionManagerOptions {
  return {
    messages: [],
    currentThreadId: 1,
    sessionRoot: null,
    busy: false,
    model: "test-model",
    connRoute: () => ({
      kind: "openai",
      baseUrl: "http://localhost:1234/v1",
      keyId: "k",
      midSystemMessages: false,
    }),
    contextCap: () => null,
    setBusy: () => {},
    setError: () => {},
    setNotice: () => {},
    enabledToolList: () => [],
    systemForModel: () => "sys",
    profileForModel: () => "",
    visionOn: false,
    toolsOn: true,
    webSearchOn: true,
    webFetchOn: true,
    codeGroupsOn: {},
    liteTools: false,
    godotToolsOn: false,
    mdWriteOn: false,
    notesOn: true,
    htmlWriteOn: false,
    codeOn: false,
    activeConn: "local",
    connBaseUrls: {},
    systemPrompt: "",
    globalSystem: "",
    systemScope: "global",
    profileShare: false,
    profileName: "",
    profilePreferred: "",
    profileAbout: "",
    maxContext: "",
    stagedFiles: [],
    ...over,
  };
}

afterEach(() => {
  cleanup();
  vi.mocked(compactThread).mockClear();
});

describe("forModel — array identity (the 0.3.2 duplicate-bubble regression)", () => {
  it("returns a fresh array even when every fold stands down", () => {
    // runTurn appends the staged-read assistant turn to forModel's result; if
    // that result IS the caller's array (memory fold + compaction both standing
    // down), the append mutates thread history in place — the exact bug that
    // persisted a ghost empty assistant bubble.
    const msgs: Message[] = [
      { role: "user", content: "hi" },
      { role: "assistant", content: "yo" },
    ];
    const snapshot = JSON.parse(JSON.stringify(msgs));
    const { result } = renderHook(() =>
      useCompactionManager(baseOpts({ messages: msgs })),
    );

    const out = result.current.forModel(msgs);
    expect(out).not.toBe(msgs);

    out.push({ role: "assistant", content: "" });
    expect(msgs).toEqual(snapshot);
    expect(msgs.length).toBe(2);
  });

  it("leaves plain turns byte-identical", () => {
    const msgs: Message[] = [
      { role: "user", content: "hi" },
      { role: "assistant", content: "yo" },
      { role: "user", content: "again" },
    ];
    const { result } = renderHook(() =>
      useCompactionManager(baseOpts({ messages: msgs })),
    );
    expect(result.current.forModel(msgs)).toEqual(msgs);
  });
});

describe("forModel — staged @-attachment framing", () => {
  it("attributes staged reads to THIS message", () => {
    const msgs: Message[] = [{ role: "user", content: "fix this" }];
    const { result } = renderHook(() =>
      useCompactionManager(
        baseOpts({ messages: msgs, stagedFiles: ["src/a.ts", "src/b.ts"] }),
      ),
    );
    const content = result.current.forModel(msgs, ["src/a.ts", "src/b.ts"])[0]
      .content;
    expect(content).toBe(
      "fix this\n\n<staged-files>\n" +
        "Inserted automatically by Outpost. The user attached the file(s) " +
        "listed below to THIS message before sending it — they are not from " +
        "an earlier turn. They have already been read for you; the contents " +
        "arrive below as tool results, as part of the current turn.\n\n" +
        "- src/a.ts\n- src/b.ts\n</staged-files>",
    );
  });

  it("does not double the blank-line gap on an empty user message", () => {
    const msgs: Message[] = [{ role: "user", content: "" }];
    const { result } = renderHook(() =>
      useCompactionManager(baseOpts({ messages: msgs })),
    );
    const content = result.current.forModel(msgs, ["note.md"])[0].content;
    expect(content.startsWith("<staged-files>")).toBe(true);
  });
});

describe("forModel — action template expansion", () => {
  it("substitutes $INPUT with the query minus the slash command", () => {
    const msgs: Message[] = [
      {
        role: "user",
        content: "/research cats",
        action: { name: "research", template: "Research topic: $INPUT" },
      },
    ];
    const { result } = renderHook(() =>
      useCompactionManager(baseOpts({ messages: msgs })),
    );
    expect(result.current.forModel(msgs)[0].content).toBe(
      "Research topic: cats",
    );
  });

  it("appends an Input section when the template has no placeholder", () => {
    const msgs: Message[] = [
      {
        role: "user",
        content: "/research cats",
        action: { name: "research", template: "Research thoroughly." },
      },
    ];
    const { result } = renderHook(() =>
      useCompactionManager(baseOpts({ messages: msgs })),
    );
    expect(result.current.forModel(msgs)[0].content).toBe(
      "Research thoroughly.\n\n## Input\ncats",
    );
  });
});

describe("forModel — compaction substitution", () => {
  const msgs: Message[] = [
    { role: "user", content: "u1" },
    { role: "assistant", content: "a1" },
    { role: "user", content: "u2" },
    { role: "assistant", content: "a2" },
  ];

  it("keeps the handover note standing on an emptied transcript (/clear)", () => {
    const { result } = renderHook(() => useCompactionManager(baseOpts()));
    act(() =>
      result.current.setCompaction({ summary: "SUMMARY", up_to: 2, at: 0 }),
    );
    const out = result.current.forModel([]);
    expect(out).toHaveLength(1);
    expect(out[0].role).toBe("system");
    expect(out[0].content).toContain("before the clear");
    expect(out[0].content).toContain("Use edit_file for targeted code modifications.");
    expect(out[0].content).toContain("SUMMARY");
  });

  it("replaces the folded prefix with the note and keeps the rest verbatim", () => {
    const { result } = renderHook(() =>
      useCompactionManager(baseOpts({ messages: msgs })),
    );
    act(() =>
      result.current.setCompaction({ summary: "SUMMARY", up_to: 2, at: 0 }),
    );
    const out = result.current.forModel(msgs);
    expect(out).toHaveLength(3);
    expect(out[0].role).toBe("system");
    expect(out[0].content).toContain("The first 2 messages");
    expect(out[0].content).toContain("Use edit_file for targeted code modifications.");
    expect(out[0].content).toContain("SUMMARY");
    expect(out[1]).toBe(msgs[2]);
    expect(out[2]).toBe(msgs[3]);
  });

  it("uses the standing-note wording when up_to folded nothing", () => {
    const { result } = renderHook(() =>
      useCompactionManager(baseOpts({ messages: msgs })),
    );
    act(() =>
      result.current.setCompaction({ summary: "SUMMARY", up_to: 0, at: 0 }),
    );
    expect(result.current.forModel(msgs)[0].content).toContain(
      "covers earlier work done in this session",
    );
  });

  it("clamps up_to to the surviving transcript length", () => {
    const { result } = renderHook(() =>
      useCompactionManager(baseOpts({ messages: msgs })),
    );
    act(() =>
      result.current.setCompaction({ summary: "SUMMARY", up_to: 99, at: 0 }),
    );
    const out = result.current.forModel(msgs);
    expect(out).toHaveLength(1);
    expect(out[0].role).toBe("system");
  });

  it("preserves trailing user prompt even if up_to is stale after clear", () => {
    const { result } = renderHook(() => useCompactionManager(baseOpts()));
    act(() =>
      result.current.setCompaction({ summary: "SUMMARY", up_to: 4, at: 0 }),
    );
    const out = result.current.forModel([
      { role: "user", content: "first after clear" },
    ]);
    expect(out).toHaveLength(2);
    expect(out[0].role).toBe("system");
    expect(out[0].content).toContain("SUMMARY");
    expect(out[1]).toEqual({ role: "user", content: "first after clear" });
  });
});

describe("runCompaction — boundary selection", () => {
  it("refuses (and never calls the backend) with too few user turns", () => {
    const setError = vi.fn();
    const { result } = renderHook(() =>
      useCompactionManager(
        baseOpts({
          messages: [{ role: "user", content: "only one" }],
          setError,
        }),
      ),
    );
    act(() => {
      result.current.runCompaction();
    });
    expect(setError).toHaveBeenCalledWith(
      "Not enough history to compact yet — have a few more turns first.",
    );
    expect(compactThread).not.toHaveBeenCalled();
    expect(result.current.isCompacting).toBe(false);
  });

  it("sets isCompacting to true while compacting and false when finished", async () => {
    let resolveCompact!: (val: { summary: string; dropped: number }) => void;
    vi.mocked(compactThread).mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolveCompact = resolve;
        }),
    );

    const { result } = renderHook(() =>
      useCompactionManager(
        baseOpts({
          messages: [
            { role: "user", content: "first" },
            { role: "assistant", content: "ok" },
            { role: "user", content: "second" },
          ],
        }),
      ),
    );

    expect(result.current.isCompacting).toBe(false);

    let compactPromise: Promise<void>;
    act(() => {
      compactPromise = result.current.runCompaction();
    });

    expect(result.current.isCompacting).toBe(true);

    await act(async () => {
      resolveCompact({ summary: "Folded history", dropped: 0 });
      await compactPromise;
    });

    expect(result.current.isCompacting).toBe(false);
    expect(result.current.compaction?.summary).toBe("Folded history");
  });
});

