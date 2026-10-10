# Outpost

**A fast, local-first AI development harness engineered for Windows.**

Outpost brings modern agentic coding workflows directly to the Windows desktop. Built with **Tauri v2**, **Rust**, and **React**, it pairs local or hosted language models with a rich native toolset, interactive approval gates, and deep project awareness — with zero WSL or virtualization overhead.

---

## Why Outpost?

Most AI coding harnesses treat Windows as an afterthought, relying on WSL bridges, slow container virtualization, or remote cloud servers. Outpost is purpose-built for developers who build, test, and ship directly on Windows — particularly **game developers, systems programmers, and native toolchain users**.

- **Native Windows Execution:** Runs commands and background processes directly in your native environment (PowerShell, Command Prompt, MSVC, Cargo, Godot, Unity, CMake).
- **Model-Agnostic Freedom:** Seamlessly switch between local models running in LM Studio or Ollama and hosted providers (Anthropic Claude, OpenAI, Groq, Google Gemini, OpenRouter, DeepSeek, Mistral).
- **Small-Model Resilient:** Engineered with concise tool schemas, 0ms slip interception, and affirmative turn receipts, allowing 4B–8B local models to execute multi-turn engineering tasks with the reliability typically reserved for massive frontier models.
- **Privacy & Local Ownership:** Conversations, notes, and project metadata are stored on your machine in a clean, local SQLite database (`%APPDATA%\com.outpost.app\outpost.db`). API keys are stored exclusively in the Windows Credential Manager.
- **Human-in-the-Loop Safety:** Every file modification, deletion, and shell command passes through an explicit diff preview and approval gate.

---

## Download

Outpost is packaged as a native 64-bit Windows desktop application with cryptographically verified auto-updates:

| Package | Format | Description |
|---|---|---|
| **[Download Setup (.exe)](https://github.com/OutpostCoding/Outpost/releases/latest)** | NSIS Installer (`x64-setup.exe`) | **Recommended.** Includes desktop integration and verified in-app auto-updates. |
| **[Download MSI (.msi)](https://github.com/OutpostCoding/Outpost/releases/latest)** | Windows Installer (`.msi`) | Enterprise-friendly MSI package for standard or silent machine installation. |

> [!NOTE]
> Releases and `.sig` Minisign cryptographic verification signatures are published on the **[GitHub Releases](https://github.com/OutpostCoding/Outpost/releases)** page.
> As an indie open-source tool without an enterprise Authenticode certificate, Windows SmartScreen may show an *"Unknown publisher"* prompt on first launch until download reputation builds. Click **More info &rarr; Run anyway** to proceed.

---

## Two Working Surfaces

Outpost is split into two dedicated surfaces, each optimized for a distinct workflow:

### 1. The Chat Surface (Ideation, Research & Notes)
Designed for conversational exploration, documentation drafting, research, and quick data calculations.

* **Where Files Live:** All notes and web pages authored on the chat surface are stored in Outpost's dedicated notes vault on your machine at:
  ```
  %APPDATA%\com.outpost.app\notes\
  ```
* **Chat Tools Available:**
  * **Notes (`notes` toggle):** `md_write`, `md_edit`, `read_note`, and `search_notes` (discover and edit persistent project notes with full BM25 search).
  * **Pages (`html-write` toggle):** `html_write`, `html_edit`, `read_note`, and `search_pages` (author standalone HTML/CSS mockups, interactive tools, and prototypes with polite on-demand browser preview chips upon completion).
  * **Sandboxed Python (`python` toggle):** `run_python` runs calculations, simulation scripts, and data transforms inside an offline WebAssembly Python runtime (Pyodide). Requires no local Python installation, standard library only, with zero filesystem or network access.
  * **Web (`web-search` & `web-fetch` toggles):** Real-time web search and article extraction. **Requires a Tavily API key** (free at [tavily.com](https://tavily.com)), configured in the Settings modal or via `TAVILY_API_KEY`.

---

### 2. The Code Surface (Project Coding & System Operations)
Rooted in an active project directory on disk. The model can inspect codebases, execute refactors, run test suites, and supervise long-running background services.

* **Where Files Live:** Operations read and write directly to the active project folder on your Windows filesystem. All destructive file writes, edits, and deletions are automatically snapshotted for instant session rollback (`restore_file`).
* **Core Toolset (Always On):**
  * **Inspection:** `project_overview`, `list_dir`, `read_file`, `read_file_lines`.
  * **Code Discovery:** `grep_search` (literal & regex search with glob filters), `code_search` (two-stage ephemeral BM25 lexical ranking + `bge-small` vector search for natural language queries), `find_symbols` (fast Tree-Sitter structural outlines).
  * **AST Code Editing:** `edit_file` (multi-tier fuzzy and anchored snippet matching with automatic indentation harmonization, symbol-scoped replacements, and pre-flight Tree-Sitter AST syntax validation across 7 languages), `write_file`.
  * **Safety & Versioning:** `delete_file`, `rename_file`, `restore_file` (session undo), `get_diff` (aggregate session changes).
  * **Native Execution:** `run_command` (native PowerShell/cmd commands, synchronous up to 600s, or background services via `background: true`), `process_manage` (inspect status/logs and terminate managed background processes).
  * **Verification:** `typecheck` (fixed `cargo check`, `tsc --noEmit`, or `compileall` validation).
* **Elective Capabilities (Toggled via the composer's `+` menu):**
  * **Web:** `web_search`, `web_fetch` (requires Tavily API key).
  * **Vision:** `read_image`, `browser_screenshot` (hands screenshots, diagrams, and rendered pages to vision-capable models).
  * **Browser:** `browser_open`, `browser_read`, `browser_act`, `browser_reload`, `browser_close` (headless localhost browser automation to interact with running dev servers).
  * **Godot:** `godot_scene_tree` (pure text-based Godot 4 `.tscn` parser that resolves node hierarchies and properties without launching the engine).

---

## Slash Commands & Prompt Skills

Type `/` in the composer to access session commands and scaffolded thinking workflows:

### Built-in Session Commands

| Command | Action |
|---|---|
| `/compact` | Condense the active conversation into a structured handover note, carrying decisions forward while reclaiming context tokens. |
| `/clear` | Clear the chat transcript while retaining the active compaction handover note. |
| `/uncompact` | Discard the handover note and restore the full uncompacted transcript into view. |
| `/index` | Pre-compute the semantic vector search index for the active project in the background. |

### Prompt Actions & Scaffolded Skills
Outpost includes built-in structured engineering actions (such as `/plan`, `/audit`, `/research`, `/debug`, `/commit`, `/explain`, `/perf`, `/adr`, `/grill`).

You can easily add your own custom slash skills or modify existing ones:
* **Global Actions:** Located in `%APPDATA%\com.outpost.app\actions\`. Click **Reveal Actions Folder** in the Settings modal to open this folder in Windows Explorer.
* **Workspace Actions:** Place markdown files inside `.outpost/actions/` in your project folder to create repo-specific actions (e.g. `/build-game` or `/shader-audit`).
* **Creating a Skill:** Simply create a `.md` file with a YAML description header:
  ```markdown
  ---
  description: Brief hint shown in the slash menu
  ---
  Your custom prompt instructions here. Use $INPUT to insert whatever the user types after the command.
  ```

---

## Connecting Models

Outpost communicates through clean, normalized connection adapters configured in the Connections menu (bottom-left):

| Provider | Kind | Default Base URL | Notes |
|---|---|---|---|
| **Local** | OpenAI-compatible | `http://localhost:1234/v1` | LM Studio, Ollama, or llama.cpp (100% offline, zero network latency) |
| **OpenRouter** | OpenAI-compatible | `https://openrouter.ai/api/v1` | Universal gateway to hundreds of open & proprietary models |
| **Anthropic** | Native Messages | `https://api.anthropic.com` | Claude models (extended thinking / reasoning budget support) |
| **OpenAI** | OpenAI-compatible | `https://api.openai.com/v1` | GPT & o-series models (reasoning effort support) |
| **Google Gemini** | OpenAI-compatible | `https://generativelanguage.googleapis.com/v1beta/openai` | Gemini Flash & Pro series |
| **xAI (Grok)** | OpenAI-compatible | `https://api.x.ai/v1` | Grok series |
| **Mistral** | OpenAI-compatible | `https://api.mistral.ai/v1` | Mistral Large, Codestral |
| **DeepSeek** | OpenAI-compatible | `https://api.deepseek.com/v1` | DeepSeek chat & reasoner models |
| **Qwen (DashScope)**| OpenAI-compatible | `https://dashscope-intl.aliyuncs.com/compatible-mode/v1` | Qwen Coder & reasoning models |
| **Z.ai (GLM)** | OpenAI-compatible | `https://open.bigmodel.cn/api/paas/v4` | GLM models |
| **Xiaomi (MiMo)** | OpenAI-compatible | `https://api.mimo.xiaomi.com/v1` | Xiaomi MiMo models |
| **Inception AI** | OpenAI-compatible | `https://api.inception.ai/v1` | Ultra-low latency Mercury inference |
| **Groq** | OpenAI-compatible | `https://api.groq.com/openai/v1` | Ultra-fast LPU inference (450+ tok/s) |
| **Custom** | OpenAI-compatible | Custom URL | Any endpoint following OpenAI standard specs |

API keys are stored securely in the **Windows Credential Manager** (`service: outpost`) and never touch disk or frontend logs.

*To add or edit custom OpenAI-compatible endpoints, see [`src/CONNECTIONS.md`](src/CONNECTIONS.md).*

---

## Keyboard Shortcuts

The following keyboard controls are active within Outpost:

| Context | Key | Action |
|---|---|---|
| **Composer** | `Enter` | Send message / submit instruction |
| **Composer** | `Shift + Enter` | Insert newline |
| **Composer** | `Tab` or `Enter` | Accept highlighted `/` slash action or `@` file mention |
| **Composer** | `Escape` | Dismiss active slash or mention menu |
| **Approval Card** | `Enter` | Approve pending file change or command |
| **Approval Card** | `Escape` | Reject pending file change or command |
| **Modals & Rename** | `Enter` | Confirm / save |
| **Modals & Rename** | `Escape` | Cancel / dismiss |

---

## Data & Storage Locations

Outpost stores all configuration and application data within standard Windows application folders:

- **Database:** `%APPDATA%\com.outpost.app\outpost.db` (SQLite with WAL mode)
- **Credentials:** Windows Credential Manager (`service: outpost`)
- **Notes Vault:** `%APPDATA%\com.outpost.app\notes\`
- **Custom Actions:** `%APPDATA%\com.outpost.app\actions\`
- **Preferences:** Stored locally in webview storage (`outpost_prefs`)

---

## Getting Started & Development

### Prerequisites
- **Windows 10 or 11** (WebView2 is included by default)
- **Node.js** (LTS v18 or later)
- **Rust & MSVC Build Tools** ([rustup.rs](https://rustup.rs) with the `x86_64-pc-windows-msvc` target)

### Installation & Development

```powershell
# Clone the repository
git clone https://github.com/OutpostCoding/Outpost.git
cd Outpost

# Install frontend dependencies
npm install

# Launch in development mode (hot-reloading frontend + live Rust build)
npm run tauri dev
```

### Building the Production Installer

```powershell
# Build signed MSI and NSIS desktop installers
npm run tauri build
```
Installers will be generated in `src-tauri/target/release/bundle/`.

---

## License

GNU General Public License v3.0 (GPLv3). See [LICENSE](LICENSE) for details. Built for developers who take craftsmanship, software freedom, and privacy seriously.
