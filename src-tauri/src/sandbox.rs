// ── surface: backend ──────────────────────────────────
// Path sandbox — canonicalization guard keeping model-supplied paths inside allowed roots.
// Exports: contain (has_root, Prefix, …) · Related: tools/mod.rs (all path-taking tools)

//! The path sandbox — the one guard that keeps a model-supplied path inside a
//! root it is allowed to touch (the project folder, or the notes folder).
//!
//! It lives in its own module because it used to be eight hand-rolled copies,
//! and every copy shared the same two blind spots:
//!
//!  1. **`Path::is_absolute()` is the wrong question on Windows.** It requires a
//!     prefix *and* a root, so `/Windows/x` and `\Windows\x` are "relative" by
//!     that test — while `PathBuf::join` treats them as rooted and throws the
//!     base away. `project.join("/Windows/x")` is `C:\Windows\x`. The old guard
//!     let those straight through; `has_root()` is the question that catches
//!     them, plus a `Prefix` check for `C:\x`, drive-relative `C:x` (which
//!     resolves against the process CWD on that drive, not against `root`), and
//!     UNC `\\server\share`.
//!  2. **A lexical check cannot see a junction or symlink.** A link *inside* the
//!     root pointing outside it passes every string test there is. So the final
//!     answer is decided on the canonical form, not the spelling.
//!
//! Fails closed: anything that cannot be resolved is refused.

use std::path::{Component, Path, PathBuf};

/// Resolve `rel` inside `root`, or refuse it.
///
/// On success returns the joined (non-canonical) path, so callers keep ordinary
/// `C:\...` paths rather than the `\\?\` verbatim form — containment has already
/// been proven, and the verbatim prefix disables the normalisation some Windows
/// APIs still expect.
///
/// The error is a bare reason phrase ("must not contain '..'"), not a full
/// sentence, so each caller can keep the wording its own surface already uses.
pub fn contain(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let rel = rel.trim();
    if rel.is_empty() {
        return Err("the path is empty".into());
    }
    let p = Path::new(rel);
    if p.has_root() || p.components().any(|c| matches!(c, Component::Prefix(_))) {
        return Err("must be a relative path, not an absolute or drive-qualified one".into());
    }
    // Both spellings: `components()` only recognises `..` as a parent on a real
    // separator, and on non-Windows a backslash is an ordinary filename byte, so
    // `..\x` would parse as ONE component that the textual split still catches.
    if p.components().any(|c| matches!(c, Component::ParentDir))
        || rel.split(['/', '\\']).any(|c| c == "..")
    {
        return Err("must not contain '..'".into());
    }
    // Refuse access to internal Git repository metadata. Models must not read or mutate
    // .git/hooks, .git/config, etc., which would allow persistent host command execution.
    if p.components().any(|c| c.as_os_str().to_string_lossy().eq_ignore_ascii_case(".git")) {
        return Err("must not access the '.git' folder".into());
    }
    // Characters Windows cannot put in a filename at all. Refusing them is not
    // cosmetic: `note.txt:hidden` is an ALTERNATE DATA STREAM, which writes real
    // content that no directory listing shows, and `"` is what let a
    // model-chosen filename break out of a shell command line when one of these
    // paths was later handed to the OS. Neither can name a file a user would
    // ever see, so there is nothing legitimate to lose by rejecting them.
    #[cfg(windows)]
    if rel.contains(['<', '>', ':', '"', '|', '?', '*']) {
        return Err(r#"must not contain < > : " | ? *"#.into());
    }

    #[cfg(windows)]
    {
        const RESERVED: &[&str] = &[
            "CON", "PRN", "AUX", "NUL",
            "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9",
            "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
        ];
        for comp in p.components() {
            if let Component::Normal(os_str) = comp {
                let s = os_str.to_string_lossy();
                let stem = match s.split('.').next() {
                    Some(stem) => stem.trim(),
                    None => s.trim(),
                };
                if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(stem)) {
                    return Err(format!("'{stem}' is a Windows reserved device name"));
                }
            }
        }
    }

    let joined = root.join(rel);

    // Decide on the canonical form. A create targets a path that does not exist
    // yet, so canonicalize the deepest ancestor that DOES exist — everything
    // below it is a plain name (no `..` survived the check above), so the tail
    // cannot climb back out of wherever that ancestor really lives.
    let croot = std::fs::canonicalize(root)
        .map_err(|e| format!("the destination folder is unavailable ({e})"))?;
    let mut probe = joined.as_path();
    let anchor = loop {
        if let Ok(c) = std::fs::canonicalize(probe) {
            break c;
        }
        // If `canonicalize` failed but `symlink_metadata` succeeds, `probe` is a
        // broken/dangling symlink or junction — refuse to resolve through it.
        if std::fs::symlink_metadata(probe).is_ok() {
            return Err(format!(
                "path '{rel}' resolves through a broken symlink or junction"
            ));
        }
        match probe.parent() {
            Some(parent) => probe = parent,
            None => return Err("the path could not be resolved".into()),
        }
    };
    if !anchor.starts_with(&croot) {
        return Err("resolves outside the folder".into());
    }
    Ok(joined)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("outpost_sandbox_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn ordinary_relative_paths_resolve_inside() {
        let root = tmp("ok");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/b.md"), "x").unwrap();
        for rel in ["a.md", "sub/b.md", "sub\\b.md", "sub/new/deep.md", "brand new.txt"] {
            let got = contain(&root, rel).unwrap_or_else(|e| panic!("{rel} rejected: {e}"));
            assert!(got.starts_with(&root), "{rel} -> {}", got.display());
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rooted_and_traversing_paths_are_refused() {
        let root = tmp("deny");
        // `/x` carries a root on every platform; the rest are Windows spellings.
        let mut cases = vec![
            "",
            "   ",
            "/Windows/System32/drivers/etc/hosts",
            "../x",
            "a/../../x",
            ".git",
            ".git/config",
            ".git/hooks/pre-commit",
            r"sub\.git\hooks",
        ];
        if cfg!(windows) {
            cases.extend([
                r#"report" & calc.exe & "x.html"#, // shell-injection filename
                "notes.txt:hidden",                // alternate data stream
                "a<b.txt",
                "a|b.txt",
                r"\Windows\System32\drivers\etc\hosts", // has_root, is_absolute says false
                r"C:\Windows\x",                        // prefix + root
                "C:/Windows/x",
                r"C:x",         // drive-relative: resolves against the CWD, not root
                r"\\srv\sh\x",  // UNC
                r"..\x",
                r"a\..\..\x",
                "nul",
                "nul.txt",
                "con",
                "aux.md",
                "com1.json",
                r"sub\lpt1.txt",
            ]);
        }
        for rel in cases {
            assert!(contain(&root, rel).is_err(), "should have refused {rel:?}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The case no lexical guard can catch: a link inside the root pointing out.
    ///
    /// On Windows this uses a *junction*, not a symlink — a symlink needs
    /// elevation or Developer Mode (so that test would quietly skip on most
    /// machines and report green), while `mklink /J` needs neither and junctions
    /// are what actually turn up in a real profile.
    #[test]
    fn a_link_pointing_outside_the_root_is_refused() {
        let root = tmp("link_root");
        let outside = tmp("link_outside");
        std::fs::write(outside.join("secret.txt"), "s").unwrap();
        let link = root.join("escape");

        #[cfg(windows)]
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&outside)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
            || std::os::windows::fs::symlink_dir(&outside, &link).is_ok();
        #[cfg(not(windows))]
        let made = std::os::unix::fs::symlink(&outside, &link).is_ok();

        assert!(made, "could not create a link to test with");
        assert!(link.join("secret.txt").exists(), "the link does not resolve");
        assert!(
            contain(&root, "escape/secret.txt").is_err(),
            "a linked directory escaped the sandbox"
        );

        let _ = std::fs::remove_dir_all(&link); // remove the link before its target
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    /// The exact payloads from the review, against a root shaped like the real one.
    #[test]
    fn the_reported_escape_no_longer_escapes() {
        let root = tmp("report");
        for rel in [
            "/Users/alice/.ssh/id_rsa",
            r"\Users\alice\.ssh\id_rsa",
            "/Windows/System32/drivers/etc/hosts",
        ] {
            match contain(&root, rel) {
                Err(_) => {}
                Ok(p) => panic!("{rel} still escapes to {}", p.display()),
            }
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn dangling_symlink_or_junction_is_refused() {
        let root = tmp("dangling_root");
        let nonexistent_target = root.join("does_not_exist_anywhere");
        let link = root.join("broken_link");

        #[cfg(windows)]
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&nonexistent_target)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
            || std::os::windows::fs::symlink_dir(&nonexistent_target, &link).is_ok();
        #[cfg(not(windows))]
        let made = std::os::unix::fs::symlink(&nonexistent_target, &link).is_ok();

        if made {
            assert!(
                contain(&root, "broken_link/payload.txt").is_err(),
                "resolving through a broken symlink must be refused"
            );
            assert!(
                contain(&root, "broken_link").is_err(),
                "resolving a broken symlink must be refused"
            );
            let _ = std::fs::remove_dir(&link);
            let _ = std::fs::remove_file(&link);
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
