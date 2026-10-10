import { useState, useEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import { stopManagedProcess } from "../../lib/provider";

interface ProcessLog {
  id: number;
  line: string;
  stderr: boolean;
}

interface ProcessStarted {
  id: number;
  command: string;
}

interface ProcessStopped {
  id: number;
}

interface ProcessState {
  command?: string;
  lines: { text: string; err: boolean }[];
  stopped: boolean;
}

/**
 * ProcessKillPill: A lightweight, non-intrusive floating kill-pill and log-viewer
 * for background processes and dev servers.
 *
 * Replaces the old 250px-high permanent terminal drawer. Renders nothing (null)
 * when idle, and displays a sleek status pill in the bottom corner with a 1-click
 * stop button and an optional popover log tail.
 */
export function TerminalPane({ layoutDir: _layoutDir }: { layoutDir?: "row" | "column" } = {}) {
  const [logs, setLogs] = useState<{ [id: number]: ProcessState }>({});
  const [activeId, setActiveId] = useState<number | null>(null);
  const [showLogs, setShowLogs] = useState(false);
  const scrollRef = useRef<HTMLDivElement>(null);
  const dismissedRef = useRef<Set<number>>(new Set());

  useEffect(() => {
    const unlistenLog = listen<ProcessLog>("process_log", (event) => {
      const { id, line, stderr } = event.payload;
      if (dismissedRef.current.has(id)) {
        return; // User explicitly dismissed this task — do not recreate
      }

      setLogs((prev) => {
        const proc = prev[id];
        const procLogs = proc ? proc.lines : [];

        // Clean ANSI escape sequences
        const hasScreenClear =
          line.includes("\x1b[H") ||
          line.includes("\x1b[2J") ||
          line.includes("\x1b[1;1H");
        const cleanText = line.replace(/\x1b\[[0-9;?]*[a-zA-Z]/g, "");

        let newLines: { text: string; err: boolean }[];
        if (hasScreenClear) {
          // Screen reset for interactive animations — redraw in-place
          newLines = [{ text: cleanText, err: stderr }];
        } else {
          newLines = [...procLogs, { text: cleanText, err: stderr }].slice(-1000);
        }

        return {
          ...prev,
          [id]: { command: proc?.command, lines: newLines, stopped: proc?.stopped ?? false },
        };
      });

      setActiveId((curr) => (curr === null ? id : curr));
    });

    const unlistenStarted = listen<ProcessStarted>("process_started", (event) => {
      const { id, command } = event.payload;
      if (dismissedRef.current.has(id)) {
        return;
      }
      setLogs((prev) => ({
        ...prev,
        [id]: {
          command,
          lines: prev[id]?.lines ?? [],
          stopped: false,
        },
      }));
      setActiveId((curr) => (curr === null ? id : curr));
    });

    const unlistenStopped = listen<ProcessStopped>("process_stopped", (event) => {
      const { id } = event.payload;
      setLogs((prev) => {
        if (!prev[id]) {
          return {
            ...prev,
            [id]: {
              lines: [{ text: `[Process #${id} stopped]`, err: false }],
              stopped: true,
            },
          };
        }
        return {
          ...prev,
          [id]: {
            ...prev[id],
            stopped: true,
            lines: [
              ...prev[id].lines,
              { text: `[Process #${id} stopped]`, err: false },
            ],
          },
        };
      });
    });

    return () => {
      unlistenLog.then((f) => f());
      unlistenStarted.then((f) => f());
      unlistenStopped.then((f) => f());
    };
  }, []);

  useEffect(() => {
    if (showLogs && scrollRef.current) {
      scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
    }
  }, [logs, activeId, showLogs]);

  const procIds = Object.keys(logs)
    .map(Number)
    .sort((a, b) => a - b);

  if (procIds.length === 0) return null;

  const currentId = activeId !== null && logs[activeId] ? activeId : procIds[0];
  const currentProc = logs[currentId];
  const isStopped = currentProc?.stopped ?? false;

  const handleStop = async (id: number) => {
    try {
      await stopManagedProcess(id);
    } catch (e) {
      console.warn(`Failed to stop process #${id}:`, e);
    }
    setLogs((prev) => {
      if (!prev[id]) return prev;
      return {
        ...prev,
        [id]: {
          ...prev[id],
          stopped: true,
          lines: [
            ...prev[id].lines,
            { text: `[Process #${id} stopped by user]`, err: true },
          ],
        },
      };
    });
  };

  const handleDismiss = async (id: number) => {
    dismissedRef.current.add(id);

    // Stop process if still running
    if (!logs[id]?.stopped) {
      try {
        await stopManagedProcess(id);
      } catch {
        /* ignore */
      }
    }

    setLogs((prev) => {
      const next = { ...prev };
      delete next[id];
      return next;
    });

    if (activeId === id) {
      const remaining = procIds.filter((p) => p !== id);
      setActiveId(remaining.length > 0 ? remaining[0] : null);
      if (remaining.length === 0) {
        setShowLogs(false);
      }
    }
  };

  return (
    <div className="process-pill-container">
      {/* Floating Log Inspection Popover */}
      {showLogs && currentProc && (
        <div className="process-logs-popover">
          <div className="process-logs-header">
            <span className="process-logs-title">
              <span className={`process-dot ${isStopped ? "stopped" : "running"}`} />
              Task #{currentId} Logs ({currentProc.lines.length} lines)
            </span>
            <div className="process-logs-header-actions">
              <button
                type="button"
                className="process-logs-close-btn"
                onClick={() => setShowLogs(false)}
                title="Close logs window"
              >
                ✕
              </button>
            </div>
          </div>
          <div className="process-logs-body" ref={scrollRef}>
            {currentProc.lines.length === 0 ? (
              <div className="process-logs-empty">(No output recorded yet)</div>
            ) : (
              currentProc.lines.map((l, i) => (
                <div key={i} className={`process-log-line ${l.err ? "err" : ""}`}>
                  {l.text}
                </div>
              ))
            )}
          </div>
        </div>
      )}

      {/* Sleek Floating Kill Pill */}
      <div className="process-pill">
        <span
          className={`process-dot ${isStopped ? "stopped" : "running"}`}
          title={isStopped ? "Process stopped" : "Process running in background"}
        />

        {procIds.length > 1 ? (
          <select
            className="process-pill-select"
            value={currentId}
            onChange={(e) => setActiveId(Number(e.target.value))}
            title="Switch background task"
          >
            {procIds.map((id) => (
              <option key={id} value={id}>
                Task #{id}{logs[id]?.command ? `: ${logs[id].command}` : ""} {logs[id]?.stopped ? "(stopped)" : ""}
              </option>
            ))}
          </select>
        ) : (
          <span className="process-pill-label">
            Task #{currentId}
            {currentProc?.command ? (
              <span className="process-pill-cmd">
                : {currentProc.command.length > 25 ? currentProc.command.slice(0, 22) + "..." : currentProc.command}
              </span>
            ) : null}
            {isStopped ? <span className="stopped-badge"> (stopped)</span> : null}
          </span>
        )}

        <button
          type="button"
          className={`process-pill-btn logs ${showLogs ? "active" : ""}`}
          onClick={() => setShowLogs((prev) => !prev)}
          title={showLogs ? "Hide logs" : "View output logs"}
        >
          📋 Logs
        </button>

        {!isStopped && (
          <button
            type="button"
            className="process-pill-btn stop"
            onClick={() => handleStop(currentId)}
            title={`Terminate process for Task #${currentId}`}
          >
            ⏹ Stop
          </button>
        )}

        <button
          type="button"
          className="process-pill-btn close"
          onClick={() => handleDismiss(currentId)}
          title="Dismiss task"
        >
          ✕
        </button>
      </div>
    </div>
  );
}

export { TerminalPane as ProcessKillPill };
