# CODE_INDEX — the rules, not the inventory

Prose only: invariants, conventions, and multi-file feature threads. **No layout, no per-file
listings, no line numbers** — all of that is computed fresh (`project_info` map, `project_search`,
`code_search`, line-1 file banners), so it can never go stale.

Companion docs: `README.md` (overview + user guide), `src/CONNECTIONS.md` (providers).
Note: there is no ENV_NOTES file — the environment notes are a Rust const (`ENV_NOTES`) in
`src-tauri/src/commands.rs`, injected into every session's system prompt.

## Invariants & gotchas

- **Head/tail discipline (the prompt-cache invariant).** `commands.rs` builds ONE leading system
  message (instructions + profile + `grounding.rs` merged env + model-identity block — which now ends
  with a DAY-GRANULAR date, 0.2.17: no clock time, a per-minute value would bust the 24h prefix
  cache), then `agent.rs` `append_tail` appends the per-turn tail parts (pin change-note, pinned docs,
  each its own system message so they cap/gate independently) and the provider adapters
  fold them into the SAME leading block, in order, before the transcript. The volatile part — pinned
  docs — rides iteration 0 only and the wrap-up (the `bb32f8c` loop lesson); the pin
  change-note is stable once computed per turn, so it may ride along on every iteration. Append; never
  edit the middle. Nothing volatile (clock, tool log, approval posture, context estimate) lives in the
  tail any more — 0.2.17 removed them all.
- **Tool history is reconstructed at request time, never stored as rows.** The transcript on disk is
  user/assistant messages whose tool calls + results live inside assistant `segments`. Before every API
  call (both `send_chat` and `preview_context`, so the inspector can't drift) `commands.rs
  expand_messages_with_segments` rebuilds the assistant's `tool_calls` and injects `Role::Tool` result
  messages — a proper multi-turn tool transcript for the provider. Within a turn agent.rs appends real
  assistant/tool messages as it loops. Don't persist `Role::Tool` rows or rework the UI message shape
  to "fix" this — the reconstruction is deliberate (0.2.17).

- **Tool gating lives in the FRONTEND.** `App.tsx` `enabledToolList` is the real capability boundary:
  `definitions()` offers every tool, `agent.rs` filters to `enabled_tools` (= the frontend list), and the
  executor refuses anything not in the list. Deliberate quirks: `search_notes`
  rides `notesOn` ALONE (write-without-search keeps small models from searching-then-editing instead of
  composing); `typecheck` = fixed cargo check / tsc --noEmit, no approval prompt.
- **Approval gate.** Tools register via `approvals.next_id` and await; the UI resolves via
  `resolve_edit`. ~13 call sites across `tools/` submodules (`project.rs`, `process.rs`, `notes.rs`)
  — keep them gated. The approval overlay's full-viewport scrim physically covers the Stop button — that
  accident is load-bearing (Stop can't cancel a pending approval). Since 0.3.6 Stop *does* cancel an
  in-flight tool, but `Approvals` is a bare oneshot with no cancellation: a Stop that lands while a tool
  awaits approval strands that one registry entry — a tiny, deliberate leak. Unit tests in `approvals.rs`
  pin ID monotonicity, channel delivery, and out-of-order resolution.
- **Command gate** (`permissions.rs`). Auto-approval is decided in one place. The shell form is split
  into segments on `&&`/`;` (quote-aware) and auto-approves only if EVERY segment clears the guards
  and matches the whitelist; pipes, redirection, `$(…)`/backtick, and a lone `&` are refused. The
  `argv` form of `run_command` runs with no shell, so it is judged on its program token + path safety
  alone (the shell guards do not apply — an argument is inert data). A bare single-letter `/x` flag
  (`dir /s`) is exempt from the absolute-path rule; longer `/…` tokens are paths. Never relax
  `BLACKLIST`, or widen the flag carve-out past one letter, without a security review.
- **Transcript scroll** (`App.tsx` + `ChatSurface.tsx`). Stickiness is one `stickRef` boolean: ON while
  the reader is at the bottom, turned OFF the instant they scroll up (the `onWheel` up-handler or a
  scroll event above `BOTTOM_EPSILON`), and never flipped by our own auto-scroll — `autoScrollRef`
  swallows the echo of the `pinToBottom` scroll, which would otherwise detach the view. A
  `ResizeObserver` on `.transcript-inner` re-pins after late layout (an approval card, a growing code
  block). Keep the slack tiny: a wide "near the bottom" threshold is what makes detaching feel grabby.
- **File banners.** Every file starts with a 3-line banner — `// ── surface: X ──` (chat / code /
  shared / backend), a one-line purpose, `Exports: … · Related: …`. Update it when the role changes;
  keep the surface tag exact (`project_search "surface:"` lists them all).
- **Commenting standard.** WHY over WHAT — never restate the code. Document every field on an options
  interface (what it gates; if it feeds the preview effect, add "must stay in the deps array" — the
  `notesOn` bug class). Section banners in large files. ~10% of lines earn a WHY comment; the rest
  earn nothing.
- **Barrel `index.ts` per folder**; `components.tsx` is a legacy shim.
- **Components: props in, JSX out.** Domain logic lives in the hooks (`use*Manager`); App.tsx is the
  composition root + turn-engine hub. The hub (`runTurn`) was deliberately NOT extracted (dependency
  crossroads; extracting traded readability for line count). When extracting an effect into a hook,
  re-verify its dependency list survives — a dropped dep makes the UI silently stale (invisible to tsc).
- **Thin frontend.** The webview never calls the backend directly — no Rust imports in the UI, only
  Tauri commands, with normalized events over a `Channel`. Keep it that way: the webview stays
  portable, and Rust stays the single authority over state.
- **The `optsRef` pattern** (App.tsx): App calls `setOptions(...)` every render so hook handlers read
  fresh deps via `optsRef.current ?? {}` — avoids threading ~15 setters; `setOptions`-during-render is
  deliberate, but no comment explains it.
- **Windows quoting & execution.** `run_command` has two forms — `argv` (spawns directly, no shell;
  use for anything with spaces/quotes) and `command` (PowerShell string). Background services (dev servers,
  watchers) run via `run_command(background: true)` and are inspected/stopped via `process_manage`.
  `run_command` intercepts slips
  (`python`→`py`, `ls`→`dir`, `grep`→`project_search`). See `ENV_NOTES` in commands.rs.
- **AST-backed editing & syntax safety.** `project_edit` uses a multi-tier snippet matcher (exact,
  whitespace-insensitive, comment-tolerant, top/bottom anchor, sliding-window similarity) with automatic
  indentation harmonization. Passing `symbol` scopes the search inside a named function/class to disambiguate
  repeated lines, or replaces whole functions/methods/classes cleanly. Pre-flight in-memory Tree-Sitter
  AST syntax validation (`check_syntax`) surfaces diagnostic warnings in the approval diff modal before corruption
  can be committed to disk. Warnings fire only when an edit *introduces* an error (the check compares against
  the pre-edit content), so a file with pre-existing parse quirks never nags on every save.
- **Sandbox.** Paths must pass `sandbox.rs` canonicalization; junction tests exist — don't weaken.
  `Path::is_absolute()` is the wrong question on Windows.
- **Undo.** `snapshots.rs` covers non-git folders (session-scoped); Outpost itself is git-tracked, so
  `project_restore` ≈ git checkout for repo files.
- **Store durability.** `parking_lot::Mutex` (non-poisoning) + WAL/`busy_timeout` so one panic can't
  wedge the store and a long save doesn't block readers. Transactional writes and monotonic ID
  counters ensure database consistency.
- **Release entry point.** `main.rs` sets "no console window on Windows in release" — do not remove.
- **Commits are left to the model** — the model commits its own work as it goes, so it keeps its own record of what changed.
- **Version bumps** touch `package.json`, `Cargo.toml`, AND `tauri.conf.json` together.
- **Auto-update.** `tauri-plugin-updater` + `@tauri-apps/plugin-updater`; manifest is `latest.json`
  COMMITTED at the repo root (fetched via the GitHub contents API — browser download URLs 404 for
  private repos). `npm run publish:release` (scripts/publish-release.js) automates the whole flow
  (verify signed bundles → tag `vX.Y.Z` → create release → upload 4 files → regenerate `latest.json`
  → commit + push → re-sync the tag); `scripts/generate-update-manifest.js` remains as the lower-level
  step it replaced. Signed builds need `TAURI_SIGNING_PRIVATE_KEY` + password; publish needs
  `GITHUB_TOKEN`. Full story: archive/NOTES_OLD.md (2026-08-14 sections).
- **Secrets.** Keys live in the OS credential store (`keystore.rs`, Windows Credential Manager); the UI
  only ever learns "a key is set" — except `get_update_token`, the ONE command that deliberately
  returns a key (the updater token for private-repo GitHub updates).
- **Provider reasoning dialects.** "A wrong reasoning shape is usually IGNORED, so it fails silently"
  — check the dialect table in `provider/mod.rs` when touching streaming. Only Anthropic replays
  thinking blocks (with signature) on the wire; other providers never see them again. Historical turns
  unconditionally strip reasoning from prompt context and inspector preview across all connections (local
  and hosted alike), preventing local models (like Gemma/Llama) from re-attending to old reasoning blocks
  or looping mid-turn, while keeping reasoning visible in the live chat transcript.
- **Engine-error taxonomy (local backends).** Every LM Studio / llama.cpp engine failure wears the same
  outer wrapper — `"Engine protocol predict stream returned an error: {…}"` — so the wrapper is NOT a
  signature. `provider/engine_error.rs` is the single classification contract
  (`classify_engine_error` → `ContextExceeded` / `DiffParser` / `Other`) and the kind decides what the
  recovery pipeline may do: context overflow surfaces the actionable hint immediately (no salvage, no
  non-streaming retry — the same over-budget prompt would just fail again); diff-parser crashes may
  salvage completed calls from the error payload / `tools_acc` and fall back to non-streaming as a
  best-effort last resort; anything else surfaces as-is. The keyword lists are pinned by the fixture
  corpus `src-tauri/fixtures/engine_errors/` — real captured payloads with `# expect:`/`# source:`
  header tags, walked by `test_engine_error_fixtures`. When a server rewords an error: save the payload
  as a fixture, tag it, and only then touch the keyword lists.
- **WebView2 version pinning.** `webview2-com`/`windows` must match Tauri's (2.11.3 → 0.38.2 → 0.61.3)
  — see `capture.rs`.
- **MCP is gone (0.2.17).** Removed Sep 1 2026 (REFACTOR_NOTES.md) — no `McpManager`, no external
  tool servers, no gating layer. `src-tauri/tests/mcp_fixture.py` was deleted with it; all tools
  are native. The `vision_on` flag survives on the wire, but no backend gate reads it.

- **App State & Context.** Avoid massive prop-drilling. Global settings and transient UI state (`FilesDrawer`, `SettingsModal`) are routed through `AppContext` via `useAppContext()`. `App.tsx` instantiates the state and hooks and passes their exports into the Context Provider. Do NOT revert to manually threading 40+ props through component layers; keep components decoupled using the Context.

- **Markdown & Mermaid.** Avoid rendering heavy components (like Mermaid SVGs) during LLM token streaming, as it causes massive CPU spikes and layout thrashing. `Markdown.tsx` accepts a `streaming` prop from `TurnTimeline.tsx` to defer rendering until the stream ends. Additionally, `mermaid.render` must be wrapped in a strict try/catch that physically queries and removes orphaned SVG nodes (`[id^="dmermaid-"]`) from `document.body` to prevent "bomb" icon DOM leaks on invalid syntax.

## Feature threads (multi-file — single-file lookups are `project_search`/`code_search`'s job)

- **Compaction** → `commands.rs` (`COMPACTION_PROMPT`, `compact_thread`, `render_for_summary`,
  `CompactResult`) + `hooks/useCompactionManager.ts` (`forModel`, `runCompaction`) + `App.tsx`
  (`onSend` `/compact` route, `deleteMessage` boundary shift) + `hooks/useFileManager.ts`
  (`openCompaction`, `saveNote` viewerKind branch)
- **Documents** → `documents.rs` (upload .docx/.pdf → markdown) + `docgen.rs` (markdown → themed
  .docx) + `commands.rs` + `hooks/useFileManager.ts` (upload/export wiring)
- **Vision** → `capture.rs` (perception-window screenshot) + `tools/mod.rs` (`queue_image` rail,
  `browser_screenshot`, `project_read_image`) + `provider/anthropic.rs` (image data-URI split/rejoin)
  + `App.tsx` `visionOn` gate (one switch rides both seeing tools)
- **Godot** → `tscn.rs` (parser) + `tools/mod.rs` (`godot_scene_tree`) — text-based; the MCP
  live-editor bridge was removed 0.2.17
- **AST Superpowers & Symbols** → `src-tauri/src/tools/symbols/` (`locator.rs`, `syntax.rs`, `registry.rs`,
  language grammars) + `src-tauri/src/tools/matcher.rs` (`resolve_snippet_edit`, `resolve_symbol_edit`,
  `harmonize_snippet_indent`) + `src-tauri/src/tools/project.rs` (`syntax_warning`, `code_symbols`) — in-memory
  Tree-Sitter syntax checking (<1ms), automatic indentation harmonizer across snippet match tiers, symbol-scoped
  snippet disambiguation, and wholesale symbol replacement across 7 languages (Rust, TS/JS, Python, Go, GDScript, C#).
- **Backend Tools Modularization** → `src-tauri/src/tools/` submodules: `context.rs` (head/tail truncation,
  capped line numbering, diff formatting), `matcher.rs` (multi-tier snippet matching, AST symbol edits),
  `definitions.rs` (tool schemas), `process.rs` (command execution, background processes), `notes.rs` (notes,
  markdown/HTML editing), `web.rs` (search, fetch, browser automation), and `project.rs` (file I/O, search,
  syntax validation). `tools/mod.rs` (337 lines) serves purely as the composition root, `Tools` struct,
  and `execute()` router.
- **Missing project folder** → `commands.rs` (`check_directory_exists` / `check_directories_exist`, plus the
  `send_chat` folder guard) + `hooks/useFileManager.ts` (`folderMissing`) + `App.tsx` (`missingRoots`,
  re-checked on mount + window focus) + `FilesDrawer.tsx` (relocate panel) + `Sidebar.tsx` (⚠️ root flag).
  A folder that has moved or been deleted must never read as "empty project".
- **Turn cancellation (Stop)** → `agent.rs` (`run_turn` races `tools.execute` against
  `cancel.cancelled()` in a biased `tokio::select!`) + `commands.rs` (`stop_turn` returns whether a
  live token was cancelled; the token slot is cleared by `ResetOnDrop`) + `tools/code_search.rs`
  (token checkpoints at entry / per corpus file / pre-vector / per `EMBED_BATCH` embed batch; aborts
  before any cache write) + `hooks/useTurnRunner.ts` (`isStopping`, set only when `stop_turn`
  confirms, reset on the `busy` edge) + `App.tsx`/`Composer.tsx`/`ChatSurface.tsx` (the "Stopping…"
  button). Dropping the tool future only cancels it at its next await point — detached
  `spawn_blocking` work stops via its own token checks.
- **Local tool-call resilience (LM Studio / llama.cpp)** → `provider/engine_error.rs` (error taxonomy,
  the kind → recovery contract) + `provider/lmstudio.rs` (`extract_valid_json_args` permissive argument
  recovery for trailing-token garbage, `tools_acc` accumulator salvage mid-stream with `ToolStart`
  dedup, `salvage_tools_from_diff_error` payload scraping, sequential-call truncation) +
  `fixtures/engine_errors/` (captured error payloads). Small local models (Ling, Spark, Qwen) emit
  malformed or multi-call tool JSON and trip llama.cpp's token diff parser; every recovery path here
  exists because one did.
