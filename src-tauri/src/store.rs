// ── surface: backend ──────────────────────────────────
// SQLite persistence for threads/messages — flat ordered list, WAL, non-poisoning mutex.
// Exports: Store (open, list_threads, save_thread, …), ThreadInfo · Related: commands.rs

//! SQLite persistence for threads and messages (spec §8). A flat, ordered
//! message list (no tree — §9 uses duplicate-not-fork). One file holds it all.

use std::path::Path;
use parking_lot::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

use crate::provider::{Message, Role, Source, ToolCall};

#[derive(Serialize)]
pub struct ThreadInfo {
    pub id: i64,
    pub title: String,
    pub updated_at: i64,
    /// The project folder a coding session is bound to, pulled from the thread's
    /// config blob. `None` for ordinary chat threads.
    pub project_root: Option<String>,
}

pub struct Store {
    conn: Mutex<Connection>,
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn role_to_str(role: &Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn role_from_str(s: &str) -> Role {
    match s {
        "system" => Role::System,
        "assistant" => Role::Assistant,
        "tool" => Role::Tool,
        _ => Role::User,
    }
}

impl Store {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "foreign_keys", true)?;
        // WAL + NORMAL so a long save stops blocking readers, and a second writer
        // (a double-invoke, or a future background consumer) waits up to the
        // busy_timeout instead of erroring. The single shared connection means
        // only one writer exists today, but this is cheap insurance.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS threads (
                id INTEGER PRIMARY KEY,
                title TEXT NOT NULL DEFAULT 'New chat',
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY,
                thread_id INTEGER NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
                ord INTEGER NOT NULL,
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                tool_calls TEXT,
                tool_call_id TEXT,
                created_at INTEGER NOT NULL,
                reasoning TEXT,
                sources TEXT,
                segments TEXT,
                images TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_messages_thread ON messages(thread_id, ord);",
        )?;
        // Migrate pre-existing DBs that predate later columns.
        let _ = conn.execute("ALTER TABLE messages ADD COLUMN reasoning TEXT", []);
        let _ = conn.execute("ALTER TABLE messages ADD COLUMN sources TEXT", []);
        let _ = conn.execute("ALTER TABLE messages ADD COLUMN segments TEXT", []);
        let _ = conn.execute("ALTER TABLE messages ADD COLUMN images TEXT", []);
        let _ = conn.execute("ALTER TABLE threads ADD COLUMN config TEXT", []);
        let _ = conn.execute("ALTER TABLE threads ADD COLUMN deleted_at INTEGER", []);
        // High-water mark for thread ids — see `create_thread`. A plain
        // `INTEGER PRIMARY KEY` reuses the rowid of a purged top row; monotonic
        // IDs prevent new conversations from colliding with past exported IDs.
        // `AUTOINCREMENT` is the textbook fix but needs a full table rebuild with a
        // live foreign key hanging off it; this gets the same guarantee additively.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value INTEGER NOT NULL);",
        )?;
        // Seed from the existing maximum so an upgrade can't hand out an id that
        // is already in use.
        let _ = conn.execute(
            "INSERT OR IGNORE INTO meta (key, value)
             SELECT 'max_thread_id', COALESCE(MAX(id), 0) FROM threads",
            [],
        );
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn list_threads(&self) -> anyhow::Result<Vec<ThreadInfo>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, title, updated_at, json_extract(config, '$.project_root') FROM threads
             WHERE deleted_at IS NULL ORDER BY updated_at DESC",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(ThreadInfo {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    updated_at: r.get(2)?,
                    project_root: r.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    #[cfg(test)]
    fn open_for_test(path: &Path) -> Self {
        Self::open(path).unwrap()
    }

    /// Create a thread with an id that has NEVER been used before.
    ///
    /// Not `last_insert_rowid()` on a bare `INTEGER PRIMARY KEY`: SQLite hands
    /// back `max(id) + 1`, so purging the newest chat frees its id for the next
    /// one. The high-water mark only ever goes up so IDs are never reissued.
    pub fn create_thread(&self, title: &str) -> anyhow::Result<i64> {
        let now = now_millis();
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let high: i64 = tx.query_row(
            "SELECT MAX(v) FROM (SELECT value AS v FROM meta WHERE key = 'max_thread_id'
                                 UNION ALL SELECT COALESCE(MAX(id), 0) FROM threads)",
            [],
            |r| r.get(0),
        )?;
        let id = high + 1;
        tx.execute(
            "INSERT INTO threads (id, title, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
            params![id, title, now],
        )?;
        tx.execute(
            "INSERT INTO meta (key, value) VALUES ('max_thread_id', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![id],
        )?;
        tx.commit()?;
        Ok(id)
    }

    pub fn load_messages(&self, thread_id: i64) -> anyhow::Result<Vec<Message>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT role, content, tool_calls, tool_call_id, created_at, reasoning, sources, segments, images
             FROM messages WHERE thread_id = ?1 ORDER BY ord",
        )?;
        let rows = stmt
            .query_map(params![thread_id], |r| {
                let role: String = r.get(0)?;
                let content: String = r.get(1)?;
                let tool_calls: Option<String> = r.get(2)?;
                let tool_call_id: Option<String> = r.get(3)?;
                let created_at: i64 = r.get(4)?;
                let reasoning: Option<String> = r.get(5)?;
                let sources: Option<String> = r.get(6)?;
                let segments: Option<String> = r.get(7)?;
                let images: Option<String> = r.get(8)?;
                Ok((
                    role,
                    content,
                    tool_calls,
                    tool_call_id,
                    created_at,
                    reasoning,
                    sources,
                    segments,
                    images,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let messages = rows
            .into_iter()
            .map(
                |(role, content, tc, tcid, created_at, reasoning, sources, segments, images)| Message {
                    role: role_from_str(&role),
                    content,
                    images: images
                        .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
                        .unwrap_or_default(),
                    tool_calls: tc
                        .and_then(|s| serde_json::from_str::<Vec<ToolCall>>(&s).ok())
                        .unwrap_or_default(),
                    tool_call_id: tcid,
                    created_at: Some(created_at),
                    reasoning,
                    reasoning_signature: None,
                    sources: sources
                        .and_then(|s| serde_json::from_str::<Vec<Source>>(&s).ok())
                        .unwrap_or_default(),
                    segments: segments
                        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                },
            )
            .collect();
        Ok(messages)
    }

    /// Replace a thread's messages and bump its title/timestamp. Simpler than
    /// incremental inserts and fine for personal-scale threads.
    pub fn save_thread(
        &self,
        thread_id: i64,
        title: &str,
        messages: &[Message],
    ) -> anyhow::Result<()> {
        let now = now_millis();
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM messages WHERE thread_id = ?1", params![thread_id])?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO messages
                 (thread_id, ord, role, content, tool_calls, tool_call_id, created_at, reasoning, sources, segments, images)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            )?;
            for (ord, m) in messages.iter().enumerate() {
                let tool_calls = if m.tool_calls.is_empty() {
                    None
                } else {
                    Some(serde_json::to_string(&m.tool_calls)?)
                };
                let sources = if m.sources.is_empty() {
                    None
                } else {
                    Some(serde_json::to_string(&m.sources)?)
                };
                let segments = m
                    .segments
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?;
                let images = if m.images.is_empty() {
                    None
                } else {
                    Some(serde_json::to_string(&m.images)?)
                };
                stmt.execute(params![
                    thread_id,
                    ord as i64,
                    role_to_str(&m.role),
                    m.content,
                    tool_calls,
                    m.tool_call_id,
                    m.created_at.unwrap_or(now),
                    m.reasoning,
                    sources,
                    segments,
                    images
                ])?;
            }
        }
        // Bump the timestamp always; only auto-name a still-unnamed thread, so a
        // manual rename (see `rename_thread`) sticks past later saves.
        tx.execute(
            "UPDATE threads SET updated_at = ?1 WHERE id = ?2",
            params![now, thread_id],
        )?;
        tx.execute(
            "UPDATE threads SET title = ?1 WHERE id = ?2 AND title = 'New chat'",
            params![title, thread_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Duplicate-not-fork (§9): copy `up_to` messages (from the start) of a
    /// thread into a brand-new independent thread. Returns the new thread id.
    pub fn duplicate_thread(&self, thread_id: i64, up_to: i64) -> anyhow::Result<i64> {
        let now = now_millis();
        let mut conn = self.conn.lock();
        let (title, config): (String, String) = conn.query_row(
            "SELECT title, COALESCE(config, '{}') FROM threads WHERE id = ?1",
            params![thread_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO threads (title, config, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
            params![format!("{title} (copy)"), config, now],
        )?;
        let new_id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO messages
             (thread_id, ord, role, content, tool_calls, tool_call_id, created_at, reasoning, sources, segments, images)
             SELECT ?1, ord, role, content, tool_calls, tool_call_id, created_at, reasoning, sources, segments, images
             FROM messages WHERE thread_id = ?2 AND ord < ?3 ORDER BY ord",
            params![new_id, thread_id, up_to],
        )?;
        tx.commit()?;
        Ok(new_id)
    }

    /// Soft-delete: move a thread to the trash. Recoverable via `restore_thread`,
    /// or permanently removed via `purge_thread`. Keeps its messages intact.
    pub fn delete_thread(&self, thread_id: i64) -> anyhow::Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE threads SET deleted_at = ?1 WHERE id = ?2",
            params![now_millis(), thread_id],
        )?;
        Ok(())
    }

    /// List trashed threads, most-recently-deleted first.
    pub fn list_trashed(&self) -> anyhow::Result<Vec<ThreadInfo>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, title, updated_at FROM threads
             WHERE deleted_at IS NOT NULL ORDER BY deleted_at DESC",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(ThreadInfo {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    updated_at: r.get(2)?,
                    project_root: None,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Restore a trashed thread back to the active list.
    pub fn restore_thread(&self, thread_id: i64) -> anyhow::Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE threads SET deleted_at = NULL WHERE id = ?1",
            params![thread_id],
        )?;
        Ok(())
    }

    /// Permanently delete one thread (cascades to its messages).
    pub fn purge_thread(&self, thread_id: i64) -> anyhow::Result<()> {
        let conn = self.conn.lock();
        conn.execute("DELETE FROM threads WHERE id = ?1", params![thread_id])?;
        Ok(())
    }

    /// Permanently empty the trash (cascades to messages).
    pub fn purge_trashed(&self) -> anyhow::Result<()> {
        let conn = self.conn.lock();
        conn.execute("DELETE FROM threads WHERE deleted_at IS NOT NULL", [])?;
        Ok(())
    }

    /// Rename a thread (manual title — sticks past auto-derivation).
    pub fn rename_thread(&self, thread_id: i64, title: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE threads SET title = ?1 WHERE id = ?2",
            params![title, thread_id],
        )?;
        Ok(())
    }

    /// Load a thread's per-chat config blob (JSON; defaults to "{}").
    pub fn load_config(&self, thread_id: i64) -> anyhow::Result<String> {
        let conn = self.conn.lock();
        let cfg: Option<String> = conn
            .query_row(
                "SELECT COALESCE(config, '{}') FROM threads WHERE id = ?1",
                params![thread_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(cfg.unwrap_or_else(|| "{}".into()))
    }

    /// Persist a thread's per-chat config blob (JSON).
    pub fn save_config(&self, thread_id: i64, config: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE threads SET config = ?1 WHERE id = ?2",
            params![config, thread_id],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod id_tests {
    use super::*;

    fn path(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir()
            .join(format!("outpost_store_{}_{name}.db", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    /// A thread id is never reused, even after the highest-numbered thread has been purged.
    #[test]
    fn a_purged_thread_id_is_never_reissued() {
        let p = path("ids");
        let a;
        let b;
        {
            let s = Store::open_for_test(&p);
            a = s.create_thread("first").unwrap();
            b = s.create_thread("second").unwrap();
            assert!(b > a, "ids must increase: {a} then {b}");
            s.purge_thread(b).unwrap();
            let c = s.create_thread("third").unwrap();
            assert!(c > b, "id {c} reused purged thread {b}");
        }
        // The mark lives on disk, not in memory: reopening must not rewind it.
        {
            let s = Store::open_for_test(&p);
            let d = s.create_thread("after reopen").unwrap();
            assert!(d > b + 1, "reopening reissued an id ({d}) at or below {}", b + 1);
        }
        let _ = std::fs::remove_file(&p);
    }
}
