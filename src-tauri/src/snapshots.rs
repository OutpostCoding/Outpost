// ── surface: backend ──────────────────────────────────
// Undo net for non-git folders — pre-mutation copies, session-scoped, project_restore.
// Exports: Snapshots (capture, restore, …) · Related: tools/mod.rs (write tools), commands.rs

//! Recoverability net for coding sessions that AREN'T under version control.
//! Before a project file is first mutated (write / edit / delete / rename), its
//! current content is copied aside; `project_restore` puts it back. This is the
//! undo you'd otherwise get from git, for ad-hoc folders that don't have it.
//!
//! Session-scoped: the store starts empty each app launch (snapshots don't
//! accumulate across runs), and captures each file at most once — the first time
//! it's touched — so a restore always returns the *original* pre-session content,
//! not an intermediate edit.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// What a file looked like before this session began.
pub enum Original {
    /// It existed; this is its pre-session content.
    Content(String),
    /// It did NOT exist before this session (it was created since).
    Absent,
}

pub struct Snapshots {
    dir: PathBuf,
    /// Why the store is unusable, if it is. Set at `open` when the snapshot
    /// folder can't be created, and checked by every `capture`.
    ///
    /// This used to be discarded, which disarmed the undo net for the WHOLE
    /// session without a word — while editor error strings still told the user to rely on it.
    unusable: Option<String>,
    /// Project-file path → the `.bak` holding its pre-edit content. An empty path
    /// means the file did NOT exist before this session (a fresh create), so a
    /// restore deletes it rather than writing content back.
    taken: Mutex<HashMap<PathBuf, PathBuf>>,
    counter: AtomicU64,
}

impl Snapshots {
    pub fn open(dir: PathBuf) -> Self {
        // Fresh each launch — snapshots are session-scoped, so don't let old ones
        // pile up in the app-data dir.
        let _ = std::fs::remove_dir_all(&dir);
        let unusable = std::fs::create_dir_all(&dir).err().map(|e| {
            format!("the snapshot folder {} couldn't be created ({e})", dir.display())
        });
        Self {
            dir,
            unusable,
            taken: Mutex::new(HashMap::new()),
            counter: AtomicU64::new(0),
        }
    }

    /// Copy a file's current content aside the FIRST time it's about to be
    /// mutated this session. No-op if already captured. A file that doesn't exist
    /// yet is recorded (with an empty snapshot path) so a later restore can delete
    /// the newly-created file.
    ///
    /// **Err means the caller must not proceed with the mutation.** A skipped
    /// capture does not merely lose the undo: `restore` then reports "wasn't
    /// modified this session" and `project_changes` omits the file entirely, so
    /// both surfaces affirmatively tell the user nothing happened to a file that
    /// was in fact overwritten. And the undo net is the whole reason
    /// `auto_project_writes` is allowed to skip the approval prompt.
    pub fn capture(&self, file: &Path) -> Result<(), String> {
        if let Some(why) = &self.unusable {
            return Err(why.clone());
        }
        if self.taken.lock().unwrap().contains_key(file) {
            return Ok(());
        }
        let snap = if file.exists() {
            let id = self.counter.fetch_add(1, Ordering::Relaxed);
            let dest = self.dir.join(format!("snap-{id}.bak"));
            std::fs::copy(file, &dest).map_err(|e| format!("couldn't copy it aside ({e})"))?;
            dest
        } else {
            PathBuf::new() // sentinel: created this session → restore removes it
        };
        self.taken.lock().unwrap().insert(file.to_path_buf(), snap);
        Ok(())
    }

    /// Restore one file to its captured pre-session content (or delete it if it
    /// was created this session). Err if the file was never captured.
    pub fn restore(&self, file: &Path) -> Result<String, String> {
        // Distinguish "you never touched this" from "the net was never armed" —
        // the first is information, the second was a confident falsehood.
        if let Some(why) = &self.unusable {
            return Err(format!("the undo net is unavailable this session — {why}"));
        }
        let snap = match self.taken.lock().unwrap().get(file) {
            Some(s) => s.clone(),
            None => return Err("wasn't modified this session — nothing to restore".into()),
        };
        if snap.as_os_str().is_empty() {
            if file.exists() {
                std::fs::remove_file(file).map_err(|e| e.to_string())?;
            }
            Ok("removed (it did not exist before this session)".into())
        } else {
            if let Some(parent) = file.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            std::fs::copy(&snap, file).map_err(|e| e.to_string())?;
            Ok("restored to its pre-session content".into())
        }
    }

    /// Every file captured this session — the candidates for a restore-all.
    pub fn captured_paths(&self) -> Vec<PathBuf> {
        self.taken.lock().unwrap().keys().cloned().collect()
    }

    /// The captured pre-session state of a file, or None if it was never touched
    /// this session. Lets a caller diff "before" against the file as it is now —
    /// which is all `project_changes` needs to reconstruct a session-wide diff.
    pub fn original(&self, file: &Path) -> Option<Original> {
        let snap = self.taken.lock().unwrap().get(file)?.clone();
        if snap.as_os_str().is_empty() {
            Some(Original::Absent)
        } else {
            std::fs::read_to_string(&snap).ok().map(Original::Content)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("outpost_snap_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn captures_and_restores() {
        let work = tmp("work");
        let snaps = Snapshots::open(tmp("snaps"));

        // Existing file: capture → edit → restore brings the original back.
        let f = work.join("a.txt");
        std::fs::write(&f, "original").unwrap();
        snaps.capture(&f).unwrap();
        std::fs::write(&f, "edited").unwrap();
        snaps.restore(&f).unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "original");

        // Capture is once-only: a re-capture after an edit still keeps the ORIGINAL.
        snaps.capture(&f).unwrap();
        std::fs::write(&f, "edited again").unwrap();
        snaps.restore(&f).unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "original");

        // A file created this session: capture (absent) → create → restore deletes.
        let g = work.join("new.txt");
        snaps.capture(&g).unwrap();
        std::fs::write(&g, "brand new").unwrap();
        assert!(g.exists());
        snaps.restore(&g).unwrap();
        assert!(!g.exists());

        // A file that was never captured can't be restored.
        assert!(snaps.restore(&work.join("untouched.txt")).is_err());

        let _ = std::fs::remove_dir_all(&work);
    }

    /// A store that couldn't arm itself must SAY so on every surface, rather than
    /// silently accepting captures and then reporting "wasn't modified this
    /// session" when the user tries to undo.
    #[test]
    fn an_unusable_store_refuses_instead_of_pretending() {
        let work = tmp("blocked_work");
        let f = work.join("a.txt");
        std::fs::write(&f, "original").unwrap();

        // A snapshot dir that cannot be created because a FILE is in its place.
        let blocker = tmp("blocked").join("not-a-dir");
        std::fs::write(&blocker, "in the way").unwrap();
        let snaps = Snapshots::open(blocker);

        let err = snaps.capture(&f).unwrap_err();
        assert!(err.contains("couldn't be created"), "{err}");
        // The undo surface must not claim the file was simply never touched.
        let restore_err = snaps.restore(&f).unwrap_err();
        assert!(restore_err.contains("undo net is unavailable"), "{restore_err}");
        assert!(snaps.captured_paths().is_empty());

        let _ = std::fs::remove_dir_all(&work);
    }

    /// A capture whose copy fails must not be recorded as taken — otherwise the
    /// entry exists, restore "succeeds", and the file is silently unrecoverable.
    #[test]
    fn a_failed_copy_is_not_recorded_as_captured() {
        let work = tmp("nocopy");
        let snaps = Snapshots::open(tmp("nocopy_snaps"));
        // A directory where a file is expected: fs::copy fails on it.
        let d = work.join("a-directory");
        std::fs::create_dir_all(&d).unwrap();
        assert!(snaps.capture(&d).is_err());
        assert!(snaps.captured_paths().is_empty());
        assert!(snaps.restore(&d).is_err());
        let _ = std::fs::remove_dir_all(&work);
    }
}
