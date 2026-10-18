// ── surface: shared ──────────────────────────────────
// In-app User Guide & FAQ documentation.
// Focused entirely on user experience, workflow, tools, models, and troubleshooting.
// Exports: USER_GUIDE_MD · Related: SettingsModal.tsx, Markdown.tsx

export const USER_GUIDE_MD = `# Outpost User Guide & FAQ

Welcome to **Outpost** — a local-first, agentic pair programming environment and AI workbench. Outpost gives frontier and local LLMs direct access to your codebase and creative workflows through a secure, gated tool harness while keeping your data private.

---

## Part I: The Two Surfaces

Outpost divides work into two distinct surfaces, toggled at the top of the sidebar. Each surface is tailored for a specific mode of thinking:

### 💬 The Chat Surface (Exploration, Documents & Sandboxed Code)
The Chat surface is completely detached from your filesystem. It has no access to your project code, making it a safe sandbox for research, drafting, and ad-hoc problem solving.

* **Python WebAssembly Sandbox**: Run self-contained Python scripts directly in an isolated browser worker (powered by Pyodide). Perfect for quick data analysis, math calculations, regex testing, and algorithmic experiments without touching your host machine.
* **Notes & Document Authoring** (\`md_write\`): Write and maintain markdown notes in a dedicated scratchpad. Export threads or notes as polished Word documents (\`.docx\`) or Markdown (\`.md\`) with one click.
* **Interactive HTML Pages** (\`html_write\`, \`html_edit\`): Build standalone HTML widgets, reports, and UI prototypes. When a page is updated, Outpost displays a polite \`[Open in Browser ↗]\` chip in chat, allowing you to preview the page in your default browser on demand without stealing window focus.
* **Document Ingestion**: Drop Word (\`.docx\`) and PDF (\`.pdf\`) files directly into the composer to extract and inspect their full text content in conversation.
* **Live Web Research** (\`web_search\`, \`web_fetch\`): Live internet search and page extraction. Outpost uses compact receipts that preserve ranked result links and document outlines for efficient re-fetching without cluttering context.
* **Interactive Architecture Diagrams**: Write Mermaid diagrams that render with vector fidelity. Click any diagram to open the **Lightbox Viewer** with smooth pan, mouse-wheel zoom, and native SVG export.

---

### 💻 The Code Surface (Repository Pair Programming)
The Code surface is your active development environment. When you attach a project folder via the folder picker, Outpost equips the model with a coordinated repository tool harness.

* **Surgical File Editing & Inspection** (\`read_file\`, \`write_file\`, \`edit_file\`):
  * **Full Writes**: Create new files or overwrite templates with \`write_file\`.
  * **Snippet & Symbol Replacement**: Modify code surgically with \`edit_file\`. Outpost uses multi-tier target-block matching with automatic indentation harmonization, AST \`symbol\` scoping/replacement, and pre-flight Tree-Sitter syntax validation across 7 languages.
  * **File & Symbol Inspection**: Read full files or specific line ranges with \`read_file\`, extract structural AST outlines via \`find_symbols\`, and inspect session diffs via \`get_diff\`.
* **Terminal Command Execution** (\`run_command\`, \`process_manage\`, \`typecheck\`):
  * Run builds, test suites, package managers, and scripts in your project environment, or run zero-prompt static verification via \`typecheck\`.
  * Long-running commands (such as dev servers or watch tasks) run as background tasks (\`background: true\`) managed via \`process_manage\` with real-time logs and interactive cancellation.
* **Code Intelligence & Search** (\`code_search\`, \`grep_search\`):
  * \`code_search\`: A two-stage hybrid search engine combining fast in-memory BM25 lexical search with dense 384-dimensional vector embeddings (\`bge-small-en-v1.5\`) running locally via ONNX Runtime, fused via Reciprocal Rank Fusion (RRF).
  * \`grep_search\`: Fast exact-string and regex search with path/glob filtering for exhaustive call-site enumeration.
* **Project Orientation** (\`project_overview\`, \`list_dir\`):
  * Generates orientation snapshots (languages, entry points, Git status, toolchains, runnable scripts) and directory trees to orient the model.

---

## Part II: The Gated Approval Safety Model

In the Code surface, the model operates under a **human-in-the-loop** security model:

1. **Inline Approval Cards**: Every file write, snippet replacement, deletion, and terminal command is intercepted and rendered as an inline card in the timeline before touching your disk.
2. **Reviewing Diffs**: Inspect colorful, side-by-side or unified diffs showing exactly what lines are being added, modified, or removed.
3. **Keyboard Shortcuts**:
   * <kbd>Enter</kbd> &rarr; **Approve**: Immediately executes the change.
   * <kbd>Esc</kbd> &rarr; **Reject**: Cancels the action and returns feedback to the model.
4. **Edit in Place**: Click any line in the diff or edit the command string directly inside the approval card to adjust the change before approving it.
5. **Configurable Permissions**: Under **Settings → Coding → Security & Permissions**, you can whitelist specific safe commands (e.g. \`cargo check\`, \`npm test\`) to streamline repetitive workflows.

---

## Part III: Models & Connectivity

Outpost connects to both offline local engines and frontier cloud providers:

### Local Models (100% Offline & Private)
* **LM Studio**: Connect via OpenAI-compatible endpoint (typically \`http://localhost:1234/v1\`).
* **Ollama**: Connect via \`http://localhost:11434/v1\`.
* **Privacy**: Zero prompt data, code, or context leaves your computer.

### Hosted Cloud Providers
* **Supported**: Anthropic Claude, OpenAI, Google Gemini, xAI (Grok), Mistral, DeepSeek, Qwen (DashScope), Z.ai (GLM), Xiaomi MiMo, XHToken (Horizon), Inception AI, Groq, and OpenRouter.
* **Keychain Security**: All API keys are encrypted directly in your operating system's native credential store (Windows Credential Manager / macOS Keychain). Keys are never committed or saved in plaintext config files.

---

## Part IV: Prompt Actions & Slash Commands

* **Prompt Actions** (\`/action\`):
  * Reusable prompt scaffolds defined in markdown files.
  * Click **Settings → General → Open Actions Folder** to add or customize \`.md\` action files.
* **Handover Compaction** (\`/compact\`):
  * Condenses long conversational history into a structured handover note, preserving task decisions and open todos while reclaiming context tokens.
* **Clear Transcript** (\`/clear\`):
  * Clears the visible message timeline while preserving the handover note and repository search index.
* **Workspace Indexing** (\`/index\`):
  * Pre-computes lexical and vector search indexes across your workspace in the background for instant retrieval.

---

## Part V: Frequently Asked Questions (FAQ)

### Q: Does Outpost send my repository code to any central Outpost server?
**A:** No. Outpost has no central cloud server or telemetry backend. All network traffic travels solely and directly between your local machine and your chosen model provider (or strictly within localhost when using LM Studio/Ollama).

### Q: Can I use Outpost completely offline (e.g. on an airplane)?
**A:** Yes! When connected to LM Studio or Ollama and with the local ONNX embedding model downloaded, every core capability—including repository editing, terminal commands, and semantic code search—runs 100% offline.

### Q: What is the difference between \`/compact\` and \`/clear\`?
**A:** \`/compact\` summarizes earlier conversation turns into a technical handover note so the model retains full context of what has been accomplished. \`/clear\` resets the transcript history while keeping that handover note intact so you can start fresh without losing past architectural decisions.

### Q: Why did a snippet replacement (\`edit_file\`) fail to apply?
**A:** Outpost uses multi-tier target matching with whitespace tolerance and AST symbol scoping to prevent accidental overwrites. If a file was modified externally or line numbers shifted, ask the model to re-read the target file with \`read_file\` and re-attempt the edit.

### Q: How do local code embeddings work?
**A:** Outpost runs the \`bge-small-en-v1.5\` ONNX model in-process to compute 384-dimensional dense vectors. You can download or verify the ~128 MB weights with one click under **Settings → Coding → Code Intelligence & Embeddings**. Once downloaded, embeddings persist locally and run entirely on CPU/GPU.

### Q: Why is my local model producing corrupted tool calls or looping?
**A:** Outpost includes an integrated tool repair engine that detects and salvages malformed JSON, raw diff blocks, or unclosed delimiters from smaller local models automatically. If a model gets stuck in a loop, you can pause or stop generation at any time with the stop button.

### Q: What happens to long-running background commands when I quit Outpost?
**A:** Outpost tracks managed background processes (such as dev servers started via \`run_command\`). When you quit the application, Outpost automatically terminates all child processes, preventing orphaned servers from squatting on your local ports.

### Q: Can Outpost extract text from PDFs and Word documents?
**A:** Yes. In the Chat surface, drag and drop any \`.pdf\` or \`.docx\` file into the composer. Outpost extracts the text content locally and passes it into the conversation context for analysis or summarization.
`;
