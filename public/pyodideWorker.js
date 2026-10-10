// Sandboxed Python runner (module worker). Loads a local, offline Pyodide build
// once, then evaluates code on demand with stdout/stderr captured. Runs off the
// main thread so the UI never freezes; the main thread terminates this worker to
// kill a runaway loop (see ../src/lib/python.ts).
import { loadPyodide } from "/pyodide/pyodide.mjs";

let buf = "";
const ready = loadPyodide({ indexURL: "/pyodide/" }).then((py) => {
  py.setStdout({ batched: (s) => append(s) });
  py.setStderr({ batched: (s) => append(s) });
  // Disable network access in the worker once Pyodide's wasm binary has finished loading.
  // This enforces the advertised "no internet access" sandbox contract.
  self.fetch = undefined;
  self.XMLHttpRequest = undefined;
  self.WebSocket = undefined;
  return py;
});

// Pyodide's `batched` stdout/stderr hands us each line with its trailing newline
// already stripped, so re-add it — otherwise every print() runs onto one line.
// This is what the MODEL sees too (not just the display), so a mangled capture
// means the model reasons over concatenated output.
function append(s) {
  buf += s + "\n";
}

self.onmessage = async (e) => {
  const { id, code } = e.data || {};
  let py;
  try {
    py = await ready;
  } catch (loadErr) {
    self.postMessage({ id, output: "Python runtime failed to load: " + String(loadErr) });
    return;
  }
  buf = "";
  try {
    const result = await py.runPythonAsync(code);
    // Echo the last expression's value, REPL-style, when it produced one and the
    // code didn't already print it.
    if (result !== undefined && result !== null) {
      let r;
      try {
        r = typeof result.toString === "function" ? result.toString() : String(result);
      } catch {
        r = String(result);
      }
      if (result && typeof result.destroy === "function") {
        try {
          result.destroy();
        } catch {
          /* ignore */
        }
      }
      if (r && r.trim() && r !== "undefined") {
        buf += (buf && !buf.endsWith("\n") ? "\n" : "") + r;
      }
    }
  } catch (err) {
    const msg = err && err.message ? err.message : String(err);
    buf += (buf && !buf.endsWith("\n") ? "\n" : "") + msg;
  }
  self.postMessage({ id, output: buf });
};
