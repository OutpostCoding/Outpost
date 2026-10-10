# Connections — adding & editing model providers

This app reaches language models through **connections**: local (LM Studio),
Anthropic (Claude), and any **OpenAI-compatible** API (OpenRouter, Mistral,
Google Gemini, Mercury, Groq, DeepSeek, Together, …). This file explains how to
add, remove, or change them yourself — no special tooling, just a text editor.

---

## The one place that defines the menu

Everything shown in the **Connections** menu (the settings button by your profile, bottom-left — there is deliberately no cog glyph)
comes from a single list:

> **`src/utils.ts`** → the `const CONNECTIONS = [ … ]` array.
> (Open the file and search for `const CONNECTIONS`.)

Each entry looks like this:

```ts
{ id: "openrouter", label: "OpenRouter", kind: "openai",
  baseUrl: "https://openrouter.ai/api/v1", needsKey: true, ready: true },
```

| field | what it means |
|---|---|
| `id` | A unique short id — lowercase, no spaces. **Also the name the API key is stored under** in the OS keychain, so don't reuse an id or rename it after a key is saved. |
| `label` | The name shown in the menu. |
| `kind` | `"openai"` for any OpenAI-compatible API (almost everything), or `"anthropic"` for Anthropic's own Claude API. |
| `baseUrl` | The API root URL. Also editable per-connection from inside the menu. |
| `needsKey` | `true` if the provider needs an API key; `false` for a local server (LM Studio). |
| `ready` | `true` = selectable in the menu. (`false` greys it out — only used to stage something unfinished.) |
| `midSystemMessages` | OBSOLETE (2026-08-30) — no longer consulted: the per-turn tail folds into the one leading system block on every adapter (CODE_INDEX.md → Invariants & gotchas → Head/tail discipline). Kept only for contract compatibility with saved connections; ignore it. |

---

## Add a provider

Almost every provider today is **OpenAI-compatible**, so adding one is a single
line — no other code changes anywhere.

1. Open `src/utils.ts` and find `const CONNECTIONS`.
2. Add an entry. Example (Together AI):

   ```ts
   { id: "together", label: "Together AI", kind: "openai",
     baseUrl: "https://api.together.xyz/v1", needsKey: true, ready: true },
   ```

3. Rebuild (see **Applying changes** below).
4. Open the Connections menu → your provider is listed → paste its API key →
   **Save** → select it. Then pick a model from the top-bar dropdown.

**Finding the base URL:** in the provider's API docs, look for their
"OpenAI-compatible" endpoint — it's the URL you'd give the OpenAI SDK as its
`base_url` / `OPENAI_BASE_URL` (usually ends in `/v1`). You can also just paste or
fix it directly in the menu's base-URL field after adding the entry.

---

## Remove a provider

Delete its line from `CONNECTIONS` and rebuild.

⚠️ This does **not** erase a stored API key — keys live in the OS keychain, not in
the code. Click **Clear** on that provider in the menu *before* removing its line,
or remove the leftover `outpost` entry by hand from Windows Credential
Manager later. (Harmless if left, just tidy.)

---

## Edit a provider

Change its `label` or `baseUrl` in the array — or, easier, just edit the base-URL
field in the menu. Menu edits are saved per-connection and persist across restarts.

---

## Applying changes

Run these from the **project root**:

- **Live app, hot-reloads on save** (the normal way to work):
  ```
  npm run tauri dev
  ```
- **Just type-check + build the web UI:**
  ```
  npm run build
  ```
- **Build a distributable desktop app:**
  ```
  npm run tauri build
  ```

If `npm run tauri dev` is already running, saving `utils.ts` reloads it
automatically — no manual step.

---

## The `kind` field — and the rare time you'd need to touch Rust

- **`"openai"`** → the shared OpenAI-compatible adapter. Covers LM Studio plus
  virtually every hosted API. Adding these is the one-line change above.
- **`"anthropic"`** → the dedicated Claude adapter (native Messages API, with
  extended thinking). Use only for Anthropic's own endpoint. The per-turn tail
  folds into the top-level `system` string on every model (CODE_INDEX.md → Invariants
  & gotchas → Head/tail discipline).

If you ever need a provider that is **neither** OpenAI-compatible nor Anthropic
(rare — most APIs speak OpenAI's format now), that requires a new adapter in Rust:

- Adapters live in `src-tauri/src/provider/` —
  `lmstudio.rs` is the OpenAI-compatible one, `anthropic.rs` is Claude.
- `src-tauri/src/provider/mod.rs` → the `make_provider()` function picks
  an adapter by `kind`. Add a new `kind` branch + a new adapter file there, using
  `anthropic.rs` as a template (implement the `Provider` trait: `stream_chat` and
  `list_models`).

---

## Where your settings and keys are stored

- **API keys:** the **OS keychain** — on Windows, **Credential Manager** (search
  for `outpost`), one entry per connection `id`. Never written to a file or
  to the app's web storage.
- **Active source + base-URL edits:** the app's browser localStorage, under the
  key `outpost_prefs` (fields `active_conn` and `conn_base_urls`).
- **Web-search key (Tavily):** entered in the same Settings menu under **Web
  search**, stored in the keychain under the id `tavily` (with the
  `TAVILY_API_KEY` env var as a fallback). Powers `web_search` and `web_fetch`;
  get a free key at tavily.com.

---

## Troubleshooting

| Symptom | Usually means |
|---|---|
| `401 Unauthorized` | Missing or wrong API key — re-enter it in the menu. |
| `404` / "not found" | Wrong base URL — check the provider's OpenAI-compatible endpoint. |
| `429 Too Many Requests` | Rate-limited; common on free models/tiers. Wait, or use a paid model. |
| "adaptive thinking is not supported on this model" | That model has no extended-thinking mode (e.g. Claude Haiku, or a non-reasoning model). Harmless — it just answers without a reasoning panel. |
| Model dropdown is empty | The source isn't reachable, or (for hosted) no key is set. For LM Studio, make sure its local server is running with a model loaded. |

---

## Notes on specific providers

- **local:** no key needed. Start its server (default
  `http://localhost:1234/v1`) and load a model; the dropdown lists what it serves.
- **Anthropic (Claude):** the model list is fetched live from your account.
  Extended thinking (the reasoning panel) works on Opus 4.6–4.8, Sonnet 4.6, and
  Fable-class models — **including during tool use** — while Haiku and older
  models simply answer directly. Since Opus 4.8 the per-turn tail (pinned docs)
  is sent as a real mid-conversation system message rather than a disguised user
  turn; older models keep the user-role shape.
- **OpenRouter:** one key, many models, including reasoning models (DeepSeek-R1,
  Qwen-thinking) whose reasoning shows in the panel. Free (`:free`) models are
  heavily rate-limited — expect frequent `429`s.

---

## The current preset list (for reference)

These ship in `CONNECTIONS` today. Base URLs are editable in the menu, and may
change over time — always trust the provider's own docs over this table.

| Provider | `kind` | base URL | key? |
|---|---|---|---|
| local | openai | `http://localhost:1234/v1` | no |
| Anthropic — Claude | anthropic | `https://api.anthropic.com` | yes |
| Xiaomi MiMo | anthropic | `https://api.xiaomimimo.com/anthropic` | yes |
| OpenRouter | openai | `https://openrouter.ai/api/v1` | yes |
| OpenAI | openai | `https://api.openai.com/v1` | yes |
| Mistral | openai | `https://api.mistral.ai/v1` | yes |
| Google Gemini | openai | `https://generativelanguage.googleapis.com/v1beta/openai` | yes |
| Z.ai (GLM) | openai | `https://api.z.ai/api/paas/v4` | yes |
| Qwen (DashScope) | openai | `https://dashscope-intl.aliyuncs.com/compatible-mode/v1` | yes |
| DeepSeek | openai | `https://api.deepseek.com/v1` | yes |
| Inception AI (Mercury) | openai | `https://api.inceptionlabs.ai/v1` | yes |
| Custom (OpenAI-compatible) | openai | *(blank — you fill it in)* | yes |

A few common extras you might add (verify the current URL in their docs):

| Provider | base URL |
|---|---|
| Groq | `https://api.groq.com/openai/v1` |
| Together AI | `https://api.together.xyz/v1` |
| Fireworks | `https://api.fireworks.ai/inference/v1` |
| Ollama (local) | `http://localhost:11434/v1` |
