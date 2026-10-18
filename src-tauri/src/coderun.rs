// ── surface: backend ──────────────────────────────────
// Round-trip registry for run_python — registers a pending run, emits RunCode, awaits
// the webview's Pyodide output via resolve_code.
// Exports: CodeRuns · Related: tools/mod.rs, commands.rs

//! Round-trip registry for sandboxed code execution. The `run_python` tool runs
//! in the Rust agent loop, but the Pyodide sandbox lives in the webview — so the
//! tool registers a pending run, emits a `RunCode` event, and awaits the output
//! here; the UI runs the code and replies via the `resolve_code` command. Same
//! shape as [`crate::approvals::Approvals`], but it carries the output text back.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use tokio::sync::oneshot;

pub struct CodeRuns {
    pending: Mutex<HashMap<u64, oneshot::Sender<String>>>,
    next: AtomicU64,
}

impl CodeRuns {
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
        }
    }

    pub fn next_id(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }

    /// Register a pending run; await the returned receiver for its output. Prunes
    /// closed oneshot senders whose receivers dropped (e.g. timeout or cancelled tool call).
    pub fn register(&self, id: u64) -> oneshot::Receiver<String> {
        let (tx, rx) = oneshot::channel();
        let mut map = self.pending.lock().unwrap();
        map.retain(|_, sender| !sender.is_closed());
        map.insert(id, tx);
        rx
    }

    /// Deliver a run's output (called from the `resolve_code` command). An
    /// unknown/dropped id is a no-op.
    pub fn resolve(&self, id: u64, output: String) {
        if let Some(tx) = self.pending.lock().unwrap().remove(&id) {
            let _ = tx.send(output);
        }
    }

    /// Clear all pending code runs, dropping their senders so any blocked
    /// receiver immediately wakes with RecvError (used when stopping a turn).
    pub fn clear(&self) {
        self.pending.lock().unwrap().clear();
    }
}
