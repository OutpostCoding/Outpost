// ── surface: backend ──────────────────────────────────
// User-approval gate — tools register a pending decision and await; the UI resolves
// via resolve_edit. Stop deliberately does NOT drain pending approvals.
// Exports: Approvals · Related: tools/mod.rs, commands.rs

//! User-approval gate for tools that must confirm before acting (spec §5 —
//! MD-write's diff-then-approve). A tool registers a pending decision and awaits
//! it; the UI replies via the `resolve_edit` command, which wakes the waiter.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use tokio::sync::oneshot;

pub struct Approvals {
    pending: Mutex<HashMap<u64, oneshot::Sender<String>>>,
    next: AtomicU64,
}

impl Approvals {
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
        }
    }

    pub fn next_id(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }

    /// Register a pending decision; await the receiver for the outcome string
    /// ("approve" | "reject" | "rename"). Prunes closed oneshot senders whose receivers
    /// dropped (e.g. timeout or cancelled tool call) to prevent memory accumulation.
    pub fn register(&self, id: u64) -> oneshot::Receiver<String> {
        let (tx, rx) = oneshot::channel();
        let mut map = self.pending.lock().unwrap();
        map.retain(|_, sender| !sender.is_closed());
        map.insert(id, tx);
        rx
    }

    /// Resolve a pending decision (called from the `resolve_edit` command). A
    /// dropped/unknown id is a no-op.
    pub fn resolve(&self, id: u64, decision: String) {
        if let Some(tx) = self.pending.lock().unwrap().remove(&id) {
            let _ = tx.send(decision);
        }
    }

    /// Clear all pending approval gates, dropping their senders so any blocked
    /// receiver immediately wakes with RecvError (used when stopping a turn).
    pub fn clear(&self) {
        self.pending.lock().unwrap().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_id_increments_monotonically() {
        let approvals = Approvals::new();
        assert_eq!(approvals.next_id(), 1);
        assert_eq!(approvals.next_id(), 2);
        assert_eq!(approvals.next_id(), 3);
    }

    #[tokio::test]
    async fn register_and_resolve_delivers_decision() {
        let approvals = Approvals::new();
        let id = approvals.next_id();
        let rx = approvals.register(id);

        approvals.resolve(id, "approve".to_string());
        let result = rx.await;
        assert_eq!(result.unwrap(), "approve");
    }

    #[test]
    fn resolve_unregistered_id_is_noop() {
        let approvals = Approvals::new();
        // Should not panic
        approvals.resolve(42, "reject".to_string());
    }

    #[test]
    fn resolve_when_receiver_dropped_is_noop() {
        let approvals = Approvals::new();
        let id = approvals.next_id();
        let rx = approvals.register(id);
        drop(rx);

        // Sending to a dropped oneshot channel safely discards without panicking
        approvals.resolve(id, "approve".to_string());
    }

    #[tokio::test]
    async fn concurrent_multiple_approvals_resolve_out_of_order() {
        let approvals = Approvals::new();
        let id1 = approvals.next_id();
        let id2 = approvals.next_id();
        let id3 = approvals.next_id();

        let rx1 = approvals.register(id1);
        let rx2 = approvals.register(id2);
        let rx3 = approvals.register(id3);

        // Resolve in reverse order with different decisions
        approvals.resolve(id3, "rename".to_string());
        approvals.resolve(id1, "approve".to_string());
        approvals.resolve(id2, "reject:No thanks".to_string());

        assert_eq!(rx1.await.unwrap(), "approve");
        assert_eq!(rx2.await.unwrap(), "reject:No thanks");
        assert_eq!(rx3.await.unwrap(), "rename");
    }
}

