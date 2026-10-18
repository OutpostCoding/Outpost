// ── surface: shared ──────────────────────────────────
// Settings — General, Connections, Coding, Profile, Updates.
// Exports: SettingsModal · Related: App.tsx, permissions.rs

import { useEffect, useState, useRef } from "react";
import { CONNECTIONS } from "../utils";
import { Switch } from "../components/ui/Switch";
import {
  revealActionsDir,
  getEmbeddingModelStatus,
  downloadEmbeddingModel,
  deleteEmbeddingModel,
  revealModelsDir,
  type EmbeddingModelStatus,
} from "../lib/provider";
import { getVersion } from "@tauri-apps/api/app";
import { useAppContext } from "../AppContext";
import { check } from "@tauri-apps/plugin-updater";
import type { Update } from "@tauri-apps/plugin-updater";
import { Markdown } from "../components/markdown/Markdown";
import { USER_GUIDE_MD } from "../docs/guide";
import { useBackdropClose } from "../hooks/useBackdropClose";

const COMMAND_PALETTES = [
  {
    id: "git",
    name: "Git & Version Control",
    commands: [
      { pattern: "git add", description: "Stage files for commit" },
      { pattern: "git commit", description: "Record changes to the repository" },
      { pattern: "git branch", description: "Create, rename, or delete branches" },
      { pattern: "git checkout", description: "Switch branches or restore working tree files" },
      { pattern: "git switch", description: "Switch branches" },
      { pattern: "git stash", description: "Stash working directory changes" },
    ],
  },
  {
    id: "node",
    name: "Node.js & Web",
    commands: [
      { pattern: "npm test", description: "Run test suite via npm" },
      { pattern: "npm run dev", description: "Start local development server" },
      { pattern: "npm run build", description: "Compile or bundle project" },
      { pattern: "tsc --noEmit", description: "TypeScript type check" },
      { pattern: "pnpm test", description: "Run test suite via pnpm" },
      { pattern: "yarn test", description: "Run test suite via yarn" },
    ],
  },
  {
    id: "rust",
    name: "Rust",
    commands: [
      { pattern: "cargo check", description: "Fast compiler check without building" },
      { pattern: "cargo test", description: "Run Rust unit and integration tests" },
      { pattern: "cargo clippy", description: "Run Rust linter" },
      { pattern: "cargo build", description: "Compile debug binaries" },
    ],
  },
  {
    id: "python",
    name: "Python",
    commands: [
      { pattern: "pytest", description: "Run Python test suite" },
      { pattern: "ruff check", description: "Fast Python linter" },
    ],
  },
];

const STANDARD_PATTERNS = new Set(COMMAND_PALETTES.flatMap((c) => c.commands.map((x) => x.pattern)));

// Read-only git inspection and shell orientation commands that are now handled
// natively (auto-approved or intercepted by native tools) and shouldn't surface as custom rules.
const BUILTIN_OR_RETIRED_PATTERNS = new Set([
  "git status",
  "git diff",
  "git log",
  "git show",
  "git blame",
  "git rev-parse",
  "git ls-files",
  "pwd",
  "ls",
  "dir",
]);

const INTERPRETER_TOKENS = new Set(["node", "python", "py", "python3", "deno", "bun", "ruby", "perl", "php"]);

function isInterpreterPattern(pattern: string): boolean {
  const first = pattern.trim().split(/\s+/)[0]?.toLowerCase();
  return Boolean(first && INTERPRETER_TOKENS.has(first));
}

export function SettingsModal({
  showSettings,
  settingsSection,
  setSettingsSection,
  onClose,
}: {
  showSettings: boolean;
  settingsSection: "general" | "connections" | "coding" | "profile" | "updates" | "guide";
  setSettingsSection: (s: "general" | "connections" | "coding" | "profile" | "updates" | "guide") => void;
  onClose: () => void;
}) {
  const {
    theme,
    setTheme,
    editMessages,
    setEditMessages,
    maxContext,
    setMaxContext,
    actions,
    maxToolCalls,
    setMaxToolCalls,
    readPruneTurns,
    setReadPruneTurns,
    permAutoWrites,
    permAutoCommands,
    permWhitelist,
    permDraft,
    setPermDraft,
    updatePerms,
    profileName,
    setProfileName,
    profilePreferred,
    setProfilePreferred,
    profileAbout,
    setProfileAbout,
    profileShare,
    setProfileShare,
    connBaseUrls,
    setConnBaseUrls,
    keyStatus,
    keyDraft,
    setKeyDraft,
    saveKey,
    deleteKey,
    availableUpdate,
    setAvailableUpdate,
  } = useAppContext();

  // Updates (Tauri updater plugin): manual check here in Settings. On Windows,
  // downloadAndInstall hands off to the installer, which relaunches the app.
  // NOTE: these hooks must stay ABOVE the `if (!showSettings) return null`
  // gate — conditional hook calls crash React ("Rendered more hooks than
  // during the previous render").
  const [appVersion, setAppVersion] = useState<string | null>(null);
  const [checking, setChecking] = useState(false);
  const [updateAvail, setUpdateAvail] = useState<Update | null>(availableUpdate);
  const [downloading, setDownloading] = useState(false);
  const [progress, setProgress] = useState<number | null>(null);
  const [updateMsg, setUpdateMsg] = useState(
    availableUpdate ? `Version ${availableUpdate.version} is available.` : ""
  );
  const backdropHandlers = useBackdropClose(onClose);
  const contentRef = useRef<HTMLDivElement>(null);

  // Sync state if launch background check completes while settings is mounted
  useEffect(() => {
    if (availableUpdate) {
      setUpdateAvail(availableUpdate);
      setUpdateMsg(`Version ${availableUpdate.version} is available.`);
    }
  }, [availableUpdate]);

  // Reset scroll to top whenever the user switches settings tabs or opens the modal
  useEffect(() => {
    if (contentRef.current) {
      contentRef.current.scrollTop = 0;
    }
  }, [settingsSection, showSettings]);

  useEffect(() => {
    let alive = true;
    getVersion()
      .then((v) => {
        if (alive) setAppVersion(v);
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, []);

  const [embedStatus, setEmbedStatus] = useState<EmbeddingModelStatus | null>(null);
  const [embedLoading, setEmbedLoading] = useState(false);
  const [embedError, setEmbedError] = useState<string | null>(null);

  const fetchEmbedStatus = () => {
    getEmbeddingModelStatus()
      .then((s) => setEmbedStatus(s))
      .catch(() => {});
  };

  useEffect(() => {
    if (showSettings && settingsSection === "coding") {
      fetchEmbedStatus();
    }
  }, [showSettings, settingsSection]);

  const [savedCustom, setSavedCustom] = useState<string[]>(() => {
    try {
      const raw = localStorage.getItem("outpost_custom_commands");
      if (raw) {
        const parsed = JSON.parse(raw);
        if (Array.isArray(parsed)) {
          return parsed.filter(
            (x) =>
              typeof x === "string" &&
              !STANDARD_PATTERNS.has(x) &&
              !BUILTIN_OR_RETIRED_PATTERNS.has(x),
          );
        }
      }
    } catch {}
    return [];
  });
  const [isManagingCustom, setIsManagingCustom] = useState(false);

  // Synchronize any newly whitelisted non-standard commands into the persistent custom tray
  useEffect(() => {
    const uncataloged = permWhitelist.filter(
      (p) => !STANDARD_PATTERNS.has(p) && !BUILTIN_OR_RETIRED_PATTERNS.has(p),
    );
    setSavedCustom((prev) => {
      let changed = false;
      const next = prev.filter((p) => {
        if (BUILTIN_OR_RETIRED_PATTERNS.has(p) || STANDARD_PATTERNS.has(p)) {
          changed = true;
          return false;
        }
        return true;
      });
      for (const u of uncataloged) {
        if (!next.includes(u)) {
          next.push(u);
          changed = true;
        }
      }
      if (changed) {
        try {
          localStorage.setItem("outpost_custom_commands", JSON.stringify(next));
        } catch {}
        return next;
      }
      return prev;
    });
  }, [permWhitelist]);

  function handleAddCustom() {
    const p = permDraft.trim();
    if (!p) return;
    if (!savedCustom.includes(p)) {
      const next = [...savedCustom, p];
      setSavedCustom(next);
      try {
        localStorage.setItem("outpost_custom_commands", JSON.stringify(next));
      } catch {}
    }
    if (!permWhitelist.includes(p)) {
      updatePerms(permAutoWrites, permAutoCommands, [...permWhitelist, p]);
    }
    setPermDraft("");
  }

  const [toDelete, setToDelete] = useState<string[]>([]);

  function handleToggleCustom(pattern: string) {
    if (permWhitelist.includes(pattern)) {
      updatePerms(
        permAutoWrites,
        permAutoCommands,
        permWhitelist.filter((x) => x !== pattern),
      );
    } else {
      updatePerms(permAutoWrites, permAutoCommands, [...permWhitelist, pattern]);
    }
  }

  function handleToggleSelectDelete(pattern: string) {
    setToDelete((prev) =>
      prev.includes(pattern) ? prev.filter((p) => p !== pattern) : [...prev, pattern],
    );
  }

  function handleFinishManage() {
    if (toDelete.length > 0) {
      const deleteSet = new Set(toDelete);
      const nextSaved = savedCustom.filter((x) => !deleteSet.has(x));
      setSavedCustom(nextSaved);
      try {
        localStorage.setItem("outpost_custom_commands", JSON.stringify(nextSaved));
      } catch {}
      const remainingWhitelist = permWhitelist.filter((x) => !deleteSet.has(x));
      if (remainingWhitelist.length !== permWhitelist.length) {
        updatePerms(permAutoWrites, permAutoCommands, remainingWhitelist);
      }
    }
    setToDelete([]);
    setIsManagingCustom(false);
  }

  // Poll while a model download/init is active
  useEffect(() => {
    if (!showSettings || settingsSection !== "coding") return;
    if (!embedLoading && embedStatus?.status !== "downloading") return;

    const interval = setInterval(() => {
      getEmbeddingModelStatus()
        .then((s) => {
          setEmbedStatus(s);
          if (s.status !== "downloading") {
            setEmbedLoading(false);
          }
        })
        .catch(() => {});
    }, 1500);

    return () => clearInterval(interval);
  }, [showSettings, settingsSection, embedLoading, embedStatus?.status]);

  const handleDownloadEmbed = async () => {
    setEmbedLoading(true);
    setEmbedError(null);
    try {
      const s = await downloadEmbeddingModel();
      setEmbedStatus(s);
    } catch (e) {
      setEmbedError(String(e));
    } finally {
      setEmbedLoading(false);
    }
  };

  const handleDeleteEmbed = async () => {
    setEmbedLoading(true);
    setEmbedError(null);
    try {
      const s = await deleteEmbeddingModel();
      setEmbedStatus(s);
    } catch (e) {
      setEmbedError(String(e));
    } finally {
      setEmbedLoading(false);
    }
  };

  if (!showSettings) return null;

  const checkForUpdates = async () => {
    setChecking(true);
    setUpdateMsg("");
    setUpdateAvail(null);
    setAvailableUpdate(null);
    setProgress(null);
    try {
      const u = await check();
      if (u) {
        setUpdateAvail(u);
        setAvailableUpdate(u);
        setUpdateMsg(`Version ${u.version} is available.`);
      } else {
        setAvailableUpdate(null);
        setUpdateMsg("You're up to date.");
      }
    } catch (e) {
      setUpdateMsg(`Couldn't check for updates: ${String(e)}`);
    } finally {
      setChecking(false);
    }
  };

  const downloadAndInstall = async () => {
    if (!updateAvail) return;
    setDownloading(true);
    setUpdateMsg("");
    let downloaded = 0;
    let contentLength: number | undefined;
    try {
      await updateAvail.downloadAndInstall((ev) => {
        if (ev.event === "Started") {
          contentLength = ev.data.contentLength;
        } else if (ev.event === "Progress") {
          downloaded += ev.data.chunkLength;
          if (contentLength) {
            setProgress(
              Math.min(100, Math.round((downloaded / contentLength) * 100)),
            );
          }
        }
      });
      setAvailableUpdate(null);
      // On Windows the app exits here; the installer relaunches it.
      setUpdateMsg("Update installed.");
    } catch (e) {
      setUpdateMsg(`Update failed: ${String(e)}`);
      setDownloading(false);
    }
  };

  return (
    <div className="settings-overlay" {...backdropHandlers}>
      <div
        className="settings-box"
        onClick={(e) => e.stopPropagation()}
        onMouseDown={(e) => e.stopPropagation()}
      >
        <button
          className="settings-close"
          onClick={onClose}
          aria-label="Close settings"
        >
          ✕
        </button>
        <aside className="settings-nav">
          <div className="settings-nav-label">Settings</div>
          {(
            [
              ["general", "General"],
              ["connections", "Connections"],
              ["coding", "Coding"],
              ["profile", "Profile"],
              ["updates", "Updates"],
              ["guide", "Guide & FAQ"],
            ] as const
          ).map(([id, label]) => (
            <button
              key={id}
              className={`settings-nav-item${settingsSection === id ? " active" : ""}`}
              onClick={() => setSettingsSection(id)}
            >
              <span>{label}</span>
              {id === "updates" && availableUpdate && (
                <span
                  className="settings-update-dot"
                  title={`Version ${availableUpdate.version} available`}
                />
              )}
            </button>
          ))}
        </aside>

        <div className="settings-content" ref={contentRef}>
          <div className="settings-page">
            {settingsSection === "general" && (
              <>
                <h2 className="settings-title">General</h2>

                <div className="settings-section">
                  <div className="settings-section-header">Appearance</div>
                  <div className="settings-group">
                    <div className="settings-row">
                      <div className="settings-row-info">
                        <span className="settings-row-label">Theme</span>
                        <span className="settings-row-hint">
                          Switch between Dark and glare-free Light mode.
                        </span>
                      </div>
                      <div className="settings-row-control">
                        <div className="theme-toggle-group">
                          <button
                            type="button"
                            className={`theme-toggle-btn ${theme === "dark" ? "active" : ""}`}
                            onClick={() => setTheme("dark")}
                          >
                            Dark
                          </button>
                          <button
                            type="button"
                            className={`theme-toggle-btn ${theme === "light" ? "active" : ""}`}
                            onClick={() => setTheme("light")}
                          >
                            Light
                          </button>
                        </div>
                      </div>
                    </div>
                  </div>
                </div>

                <div className="settings-section">
                  <div className="settings-section-header">Workflow & Context</div>
                  <div className="settings-group">
                    <div className="settings-row">
                      <div className="settings-row-info">
                        <span className="settings-row-label">Allow editing responses in place</span>
                        <span className="settings-row-hint">
                          Adds an edit button to assistant responses so you can fix typos or
                          strip lines without rerunning. Ideally used in chat-only turns — editing
                          turns with tool calls or code execution groups results at the end, which can damage model attention.
                        </span>
                      </div>
                      <div className="settings-row-control">
                        <Switch
                          checked={editMessages}
                          onChange={setEditMessages}
                        />
                      </div>
                    </div>
                    <div className="settings-row">
                      <div className="settings-row-info">
                        <span className="settings-row-label">Max context sent (tokens)</span>
                        <span className="settings-row-hint">
                          Leave empty for no cap (no nudge, no truncation, no summarising
                          clip). Set it to your model's window to get a usage nudge at 80%.
                        </span>
                      </div>
                      <div className="settings-row-control">
                        <input
                          className="conn-keyinput ctx-cap-input"
                          type="text"
                          inputMode="numeric"
                          value={maxContext}
                          spellCheck={false}
                          placeholder="unlimited"
                          onChange={(e) =>
                            setMaxContext(e.target.value.replace(/[^0-9]/g, ""))
                          }
                        />
                      </div>
                    </div>
                  </div>
                </div>

                <div className="settings-section">
                  <div className="settings-section-header">
                    <span>Prompt Actions</span>
                    <button
                      type="button"
                      className="conn-test"
                      onClick={() => void revealActionsDir()}
                    >
                      Open Actions Folder
                    </button>
                  </div>
                  <div className="settings-group">
                    <div className="settings-row vertical">
                      <span className="settings-row-hint">
                        Markdown prompt scaffolds triggered via <code>/action</code> in the composer. Add or edit <code>.md</code> files in the actions folder to create custom commands.
                      </span>
                      {actions && actions.length > 0 && (
                        <div className="perm-list" style={{ marginTop: 4 }}>
                          {actions.map((act) => (
                            <div
                              className="perm-cmd"
                              key={act.name}
                              style={{
                                display: "flex",
                                justifyContent: "space-between",
                                alignItems: "center",
                              }}
                            >
                              <span>
                                <code>/{act.name}</code>{" "}
                                <span style={{ color: "var(--muted)", fontSize: "12px" }}>
                                  — {act.description}
                                </span>
                              </span>
                              {act.scope === "workspace" && (
                                <span className="file-tag" style={{ fontSize: "10px" }}>
                                  workspace
                                </span>
                              )}
                            </div>
                          ))}
                        </div>
                      )}
                    </div>
                  </div>
                </div>
              </>
            )}

            {settingsSection === "coding" && (
              <>
                <h2 className="settings-title">Coding</h2>

                <div className="settings-section">
                  <div className="settings-section-header">Execution & Limits</div>
                  <div className="settings-group">
                    <div className="settings-row">
                      <div className="settings-row-info">
                        <span className="settings-row-label">Tool calls per turn</span>
                        <span className="settings-row-hint">
                          How many tools the model may call in one go before pausing for input. Default 40, maximum 200 (safety ceiling).
                        </span>
                      </div>
                      <div className="settings-row-control">
                        <input
                          className="conn-keyinput ctx-cap-input"
                          type="text"
                          inputMode="numeric"
                          value={maxToolCalls}
                          spellCheck={false}
                          placeholder="40"
                          onChange={(e) => {
                            const digits = e.target.value.replace(/[^0-9]/g, "");
                            if (!digits) {
                              setMaxToolCalls("");
                              return;
                            }
                            const val = parseInt(digits, 10);
                            setMaxToolCalls(val > 200 ? "200" : String(val));
                          }}
                        />
                      </div>
                    </div>
                    <div className="settings-row">
                      <div className="settings-row-info">
                        <span className="settings-row-label">Read history retention</span>
                        <span className="settings-row-hint">
                          Controls how many turns read_file and read_note outputs remain in full context before being pruned to compact receipts.
                        </span>
                      </div>
                      <div className="settings-row-control">
                        <select
                          className="conn-keyinput ctx-cap-input"
                          value={readPruneTurns ?? 0}
                          onChange={(e) => {
                            const val = parseInt(e.target.value, 10);
                            setReadPruneTurns(val === 0 ? null : val);
                          }}
                        >
                          <option value={2}>2 turns (Default)</option>
                          <option value={1}>1 turn (Aggressive)</option>
                          <option value={0}>Keep full (Unlimited)</option>
                        </select>
                      </div>
                    </div>
                  </div>
                </div>

                <div className="settings-section">
                  <div className="settings-section-header">Code Intelligence & Embeddings</div>
                  <div className="settings-group">
                    <div className="settings-row vertical">
                      <div className="settings-row-info">
                        <span className="settings-row-label">
                          Semantic search model (<code>bge-small-en-v1.5</code>)
                        </span>
                        <span className="settings-row-hint">
                          384-dimensional dense vectors running locally in-process via ONNX Runtime. Powers <code>code_search</code> and <code>/index</code>. Runs 100% offline once downloaded.
                        </span>
                      </div>

                      <div className="embed-status-card">
                        <div className="embed-status-main">
                          <div className="embed-status-indicator">
                            {embedLoading || embedStatus?.status === "downloading" ? (
                              <>
                                <span className="embed-dot downloading" />
                                <span className="embed-status-text">
                                  Downloading & initializing model weights…
                                </span>
                              </>
                            ) : embedStatus?.status === "ready" ? (
                              <>
                                <span className="embed-dot ready" />
                                <span className="embed-status-text">
                                  <strong>Ready</strong> — {embedStatus.size_formatted} on disk
                                  {embedStatus.is_loaded && (
                                    <span className="embed-loaded-tag">Active in RAM</span>
                                  )}
                                </span>
                              </>
                            ) : (
                              <>
                                <span className="embed-dot missing" />
                                <span className="embed-status-text">
                                  <strong>Not downloaded</strong> — BM25 lexical search only
                                </span>
                              </>
                            )}
                          </div>

                          <div className="embed-actions">
                            {embedLoading || embedStatus?.status === "downloading" ? (
                              <button
                                type="button"
                                className="conn-save"
                                disabled
                              >
                                Downloading…
                              </button>
                            ) : embedStatus?.status === "ready" ? (
                              <>
                                <button
                                  type="button"
                                  className="conn-clear"
                                  title="Reveal model storage folder in file manager"
                                  onClick={() => void revealModelsDir()}
                                >
                                  Reveal Folder
                                </button>
                                <button
                                  type="button"
                                  className="conn-clear"
                                  title="Re-download or verify model files"
                                  onClick={handleDownloadEmbed}
                                >
                                  Re-download
                                </button>
                                <button
                                  type="button"
                                  className="conn-clear"
                                  title="Delete model files to reclaim disk space"
                                  onClick={handleDeleteEmbed}
                                >
                                  Delete
                                </button>
                              </>
                            ) : (
                              <button
                                type="button"
                                className="conn-save"
                                onClick={handleDownloadEmbed}
                              >
                                Download Model (~128 MB)
                              </button>
                            )}
                          </div>
                        </div>

                        {embedError && (
                          <div className="embed-error-msg">
                            Error: {embedError}
                          </div>
                        )}
                      </div>
                    </div>
                  </div>
                </div>

                <div className="settings-section">
                  <div className="settings-section-header">Permissions & Safety</div>
                  <div className="settings-group">
                    <div className="settings-row">
                      <div className="settings-row-info">
                        <span className="settings-row-label">Auto-approve project file writes & edits</span>
                        <span className="settings-row-hint">
                          Sandboxed to the project folder and recoverable via git.
                          Deletes and renames always ask regardless.
                        </span>
                      </div>
                      <div className="settings-row-control">
                        <Switch
                          checked={permAutoWrites}
                          onChange={(v) => updatePerms(v, permAutoCommands, permWhitelist)}
                        />
                      </div>
                    </div>
                    <div className="settings-row">
                      <div className="settings-row-info">
                        <div className="perm-header-row">
                          <span className="settings-row-label">Auto-approve command patterns</span>
                          {permWhitelist.length > 0 && (
                            <span className={`perm-count-badge ${!permAutoCommands ? "paused" : ""}`}>
                              {permAutoCommands
                                ? `${permWhitelist.length} active`
                                : `${permWhitelist.length} saved · Paused`}
                            </span>
                          )}
                        </div>
                        <span className="settings-row-hint">
                          Read-only Git inspection (<code>status</code>, <code>diff</code>, <code>log</code>, <code>show</code>, <code>blame</code>) is always allowed automatically. Other commands matching a whitelisted pattern run without confirmation if they clear sandbox guards (no path escapes, no pipes or shell redirection, no blacklisted system tools). Compound commands require all segments to match.
                        </span>
                      </div>
                      <div className="settings-row-control">
                        <Switch
                          checked={permAutoCommands}
                          onChange={(v) => updatePerms(permAutoWrites, v, permWhitelist)}
                        />
                      </div>
                    </div>
                    <div className="settings-row vertical" style={{ borderTop: "none", paddingTop: 0 }}>
                      {/* Unified Category Trays */}
                      <div className={`perm-palette-trays ${!permAutoCommands ? "paused" : ""}`}>
                        {COMMAND_PALETTES.map((cat) => (
                          <div className="perm-tray" key={cat.id}>
                            <div className="perm-tray-header">{cat.name}</div>
                            <div className="perm-tray-chips">
                              {cat.commands.map(({ pattern, description }) => {
                                const isActive = permWhitelist.includes(pattern);
                                return (
                                  <button
                                    key={pattern}
                                    type="button"
                                    className={`perm-chip ${isActive ? "active" : ""}`}
                                    title={description ? `${pattern} — ${description}` : pattern}
                                    onClick={() => {
                                      if (isActive) {
                                        updatePerms(
                                          permAutoWrites,
                                          permAutoCommands,
                                          permWhitelist.filter((x) => x !== pattern),
                                        );
                                      } else {
                                        updatePerms(permAutoWrites, permAutoCommands, [...permWhitelist, pattern]);
                                      }
                                    }}
                                  >
                                    <span className="perm-chip-state">{isActive ? "✓" : "+"}</span>
                                    <code>{pattern}</code>
                                  </button>
                                );
                              })}
                            </div>
                          </div>
                        ))}

                        {/* 5th Tray: Custom & Project Commands */}
                        <div className="perm-tray custom-tray">
                          <div className="perm-tray-header custom-header">
                            <span>Custom & Project Commands</span>
                            {(savedCustom.length > 0 || isManagingCustom) && (
                              <button
                                type="button"
                                className={`perm-manage-btn ${isManagingCustom ? "active" : ""}`}
                                onClick={() => {
                                  if (isManagingCustom) {
                                    handleFinishManage();
                                  } else {
                                    setIsManagingCustom(true);
                                    setToDelete([]);
                                  }
                                }}
                              >
                                {!isManagingCustom
                                  ? "Manage Rules"
                                  : toDelete.length > 0
                                  ? `Delete (${toDelete.length}) & Done`
                                  : "Done"}
                              </button>
                            )}
                          </div>

                          {savedCustom.length === 0 ? (
                            <div className="perm-tray-empty">
                              No custom rules saved yet. Add project-specific scripts or one-off tools below.
                            </div>
                          ) : (
                            <div className="perm-tray-chips">
                              {savedCustom.map((pattern) => {
                                const isActive = permWhitelist.includes(pattern);
                                const isInterp = isInterpreterPattern(pattern);
                                const isSelected = toDelete.includes(pattern);
                                return isManagingCustom ? (
                                  <button
                                    key={pattern}
                                    type="button"
                                    className={`perm-chip delete-chip ${isSelected ? "selected" : ""}`}
                                    title={
                                      isSelected
                                        ? `Selected for deletion. Click to cancel.`
                                        : `Click to select for deletion.`
                                    }
                                    onClick={() => handleToggleSelectDelete(pattern)}
                                  >
                                    <code>{pattern}</code>
                                  </button>
                                ) : (
                                  <button
                                    key={pattern}
                                    type="button"
                                    className={`perm-chip ${isActive ? "active" : ""} ${
                                      isInterp ? "interpreter" : ""
                                    }`}
                                    title={
                                      isInterp
                                        ? `"${pattern}" is an interpreter that can run arbitrary code scripts in the project root`
                                        : pattern
                                    }
                                    onClick={() => handleToggleCustom(pattern)}
                                  >
                                    {isInterp && (
                                      <span
                                        className="perm-chip-badge"
                                        title="Interpreter: can run arbitrary scripts within the project root"
                                      >
                                        ⚡
                                      </span>
                                    )}
                                    <span className="perm-chip-state">{isActive ? "✓" : "+"}</span>
                                    <code>{pattern}</code>
                                  </button>
                                );
                              })}
                            </div>
                          )}

                          {/* Add Custom Command Input Row inside the tray */}
                          <div className="perm-custom-input-row">
                            <input
                              className="conn-keyinput"
                              value={permDraft}
                              spellCheck={false}
                              placeholder='Add custom command rule, e.g. "pytest tests/unit" or "node"'
                              onChange={(e) => setPermDraft(e.target.value)}
                              onKeyDown={(e) => {
                                if (e.key === "Enter") handleAddCustom();
                              }}
                            />
                            <button
                              className="conn-save"
                              onClick={handleAddCustom}
                              disabled={!permDraft.trim()}
                            >
                              Add
                            </button>
                          </div>
                        </div>
                      </div>
                    </div>
                  </div>
                </div>
              </>
            )}

            {settingsSection === "profile" && (
              <>
                <h2 className="settings-title">Profile</h2>

                <div className="settings-section">
                  <div className="settings-section-header">User Details</div>
                  <div className="settings-group">
                    <div className="settings-row">
                      <div className="settings-row-info">
                        <span className="settings-row-label">Name</span>
                        <span className="settings-row-hint">Stored locally on this device.</span>
                      </div>
                      <div className="settings-row-control">
                        <input
                          className="conn-keyinput"
                          style={{ width: 220 }}
                          value={profileName}
                          onChange={(e) => setProfileName(e.target.value)}
                          placeholder="e.g. Alex Morgan"
                        />
                      </div>
                    </div>
                    <div className="settings-row">
                      <div className="settings-row-info">
                        <span className="settings-row-label">Preferred name</span>
                        <span className="settings-row-hint">What the model should call you.</span>
                      </div>
                      <div className="settings-row-control">
                        <input
                          className="conn-keyinput"
                          style={{ width: 220 }}
                          value={profilePreferred}
                          onChange={(e) => setProfilePreferred(e.target.value)}
                          placeholder="e.g. Alex"
                        />
                      </div>
                    </div>
                    <div className="settings-row vertical">
                      <div className="settings-row-info">
                        <span className="settings-row-label">About you</span>
                        <span className="settings-row-hint">
                          What the model should know about you — role, context, how you like answers…
                        </span>
                      </div>
                      <textarea
                        className="profile-about-input"
                        value={profileAbout}
                        onChange={(e) => setProfileAbout(e.target.value)}
                        placeholder="What the model should know about you — role, context, how you like answers…"
                        rows={4}
                      />
                    </div>
                    <div className="settings-row">
                      <div className="settings-row-info">
                        <span className="settings-row-label">Share with the model</span>
                        <span className="settings-row-hint">
                          {profileShare
                            ? "Included in every chat's context."
                            : "Not shared with the model."}
                        </span>
                      </div>
                      <div className="settings-row-control">
                        <Switch
                          checked={profileShare}
                          onChange={setProfileShare}
                        />
                      </div>
                    </div>
                  </div>
                </div>
              </>
            )}

            {settingsSection === "connections" && (
              <>
                <h2 className="settings-title">Connections</h2>

                <div className="settings-section">
                  <div className="settings-section-header">Model Providers</div>
                  <div className="settings-group">
                    {CONNECTIONS.map((c) => (
                      <div key={c.id} className="conn-row">
                        <div className="conn-row-head">
                          <span className="conn-label">
                            {c.label}
                            {c.needsKey && !keyStatus[c.id] ? " — no key" : ""}
                          </span>
                        </div>
                        <input
                          className="conn-base"
                          value={connBaseUrls[c.id] ?? c.baseUrl}
                          spellCheck={false}
                          placeholder="base URL (OpenAI-compatible)"
                          onChange={(e) =>
                            setConnBaseUrls((u) => ({ ...u, [c.id]: e.target.value }))
                          }
                        />
                        {c.needsKey && (
                          <div className="conn-key">
                            <input
                              className="conn-keyinput"
                              type="password"
                              value={keyDraft[c.id] ?? ""}
                              placeholder={keyStatus[c.id] ? "••••••••" : "API key"}
                              onChange={(e) =>
                                setKeyDraft((k) => ({ ...k, [c.id]: e.target.value }))
                              }
                            />
                            <button
                              className="conn-save"
                              onClick={() => saveKey(c.id)}
                              disabled={!keyDraft[c.id]?.trim()}
                            >
                              Save
                            </button>
                            {keyStatus[c.id] && (
                              <button
                                className="conn-clear"
                                onClick={() => deleteKey(c.id)}
                              >
                                Clear
                              </button>
                            )}
                          </div>
                        )}
                      </div>
                    ))}
                  </div>
                </div>

                <div className="settings-section">
                  <div className="settings-section-header">Web Search</div>
                  <div className="settings-group">
                    <div className="settings-row vertical">
                      <div className="settings-row-info">
                        <span className="settings-row-label">Tavily Search API</span>
                        <span className="settings-row-hint">
                          Powers web_search and web_fetch. Free key at tavily.com.
                        </span>
                      </div>
                      {keyStatus["tavily"] ? (
                        <div className="conn-key">
                          <span className="conn-keyset">key set ✓</span>
                          <button
                            className="conn-clear"
                            onClick={() => deleteKey("tavily")}
                          >
                            Clear
                          </button>
                        </div>
                      ) : (
                        <div className="conn-key">
                          <input
                            type="password"
                            className="conn-keyinput"
                            value={keyDraft["tavily"] ?? ""}
                            spellCheck={false}
                            placeholder="paste Tavily key"
                            onChange={(e) =>
                              setKeyDraft((d) => ({ ...d, tavily: e.target.value }))
                            }
                            onKeyDown={(e) => {
                              if (e.key === "Enter") saveKey("tavily");
                            }}
                          />
                          <button
                            className="conn-save"
                            onClick={() => saveKey("tavily")}
                            disabled={!(keyDraft["tavily"] ?? "").trim()}
                          >
                            Save
                          </button>
                        </div>
                      )}
                    </div>
                  </div>
                </div>
              </>
            )}

            {settingsSection === "updates" && (
              <>
                <h2 className="settings-title">Updates</h2>

                <div className="settings-section">
                  <div className="settings-section-header">Version & Updates</div>
                  <div className="settings-group">
                    <div className="settings-row">
                      <div className="settings-row-info">
                        <span className="settings-row-label">
                          Outpost {appVersion ?? "…"}
                        </span>
                        <span className="settings-row-hint">
                          Updates are downloaded from GitHub Releases and verified
                          against Outpost's signing key before anything installs.
                        </span>
                      </div>
                      <div className="settings-row-control">
                        <button
                          className="conn-save"
                          disabled={checking || downloading}
                          onClick={() => void checkForUpdates()}
                        >
                          {checking ? "Checking…" : "Check for updates"}
                        </button>
                        {updateAvail && !downloading && (
                          <button
                            className="conn-save"
                            disabled={downloading}
                            onClick={() => void downloadAndInstall()}
                          >
                            Download & install {updateAvail.version}
                          </button>
                        )}
                      </div>
                    </div>
                    {(downloading || updateMsg) && (
                      <div className="settings-row">
                        <div className="settings-row-info">
                          {downloading && (
                            <span className="settings-row-hint">
                              {progress !== null
                                ? `Downloading… ${progress}%`
                                : "Downloading…"}
                            </span>
                          )}
                          {updateMsg && <span className="settings-row-hint">{updateMsg}</span>}
                        </div>
                      </div>
                    )}
                  </div>
                </div>
              </>
            )}

            {settingsSection === "guide" && (
              <>
                <h2 className="settings-title">User Guide & FAQ</h2>
                <div className="settings-guide-card">
                  <Markdown text={USER_GUIDE_MD} />
                </div>
              </>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
