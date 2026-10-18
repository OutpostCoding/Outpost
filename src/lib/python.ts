// ── surface: code ──────────────────────────────────
// Front end of the sandboxed Python runner — owns the Pyodide Web Worker (lazy, hard limits).
// Exports: preloadPython, runPython, cancelPython · Related: coderun.rs, tools/mod.rs (run_python)

// Front-end side of the sandboxed Python runner. Owns the Pyodide Web Worker:
// creates it lazily, runs code with a hard timeout, and terminates+respawns it to
// kill a runaway loop. The worker runs an offline WASM CPython (see
// ../../public/pyodideWorker.js); nothing here can touch the real filesystem.

let worker: Worker | null = null;
let counter = 0;
let activeCancel: (() => void) | null = null;

function getWorker(): Worker {
  if (!worker) {
    worker = new Worker("/pyodideWorker.js", { type: "module" });
  }
  return worker;
}

/** Kick off worker creation (and the one-time Pyodide load) ahead of first use,
 *  so the initial run doesn't pay the ~1–3s warmup. */
export function preloadPython(): void {
  getWorker();
}

/**
 * Terminate any in-flight Python execution and reset the Pyodide worker immediately
 * (called on turn stop or completion).
 */
export function cancelPython(): void {
  if (activeCancel) {
    activeCancel();
  }
}

/**
 * Run Python in the sandboxed worker; resolves with combined stdout/stderr (plus
 * the last expression's value, REPL-style). A run exceeding `timeoutMs` — e.g. an
 * infinite loop — terminates the worker (the next run reloads Pyodide fresh). A
 * worker that fails to start resolves with the error rather than hanging.
 */
export function runPython(code: string, timeoutMs = 20000): Promise<string> {
  return new Promise((resolve) => {
    const w = getWorker();
    const id = ++counter;
    let settled = false;

    const finish = (output: string, killWorker: boolean) => {
      if (settled) return;
      settled = true;
      activeCancel = null;
      clearTimeout(timer);
      w.removeEventListener("message", onMessage);
      w.removeEventListener("error", onError);
      if (killWorker) {
        try {
          w.terminate();
        } catch {
          /* ignore */
        }
        worker = null; // respawn (and reload Pyodide) on the next run
      }
      resolve(output);
    };

    activeCancel = () => {
      finish("Stopped by user.", true);
    };

    const timer = setTimeout(
      () =>
        finish(
          "⏱ Stopped — the code ran longer than 20s (possible infinite loop).",
          true,
        ),
      timeoutMs,
    );

    const onMessage = (e: MessageEvent) => {
      if (!e.data || e.data.id !== id) return;
      finish(typeof e.data.output === "string" ? e.data.output : "", false);
    };

    const onError = (e: ErrorEvent) => {
      finish(
        "Python sandbox failed to start: " + (e.message || "unknown error"),
        true,
      );
    };

    w.addEventListener("message", onMessage);
    w.addEventListener("error", onError);
    w.postMessage({ id, code });
  });
}
