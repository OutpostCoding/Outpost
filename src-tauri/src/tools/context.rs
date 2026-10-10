// ── surface: backend ──────────────────────────────────
// Context truncation, line numbering, diffing, and capped file reads.
// Exports: truncate_head_tail, format_captured, unified_diff, number_lines_capped, number_lines, read_text_capped

use std::path::Path;
use super::MAX_FETCH_CHARS;

/// Truncate long command, script, or execution output using a head+tail "sandwich".
/// Preserves the initial command context (head) and the fatal errors, assertion failures,
/// and exit summaries at the end (tail), replacing the noisy middle with an informative marker.
pub fn truncate_head_tail(text: &str, max_chars: usize) -> String {
    let char_count = text.chars().count();
    if char_count <= max_chars {
        return text.to_string();
    }

    if max_chars == 0 {
        return String::new();
    }

    // Allocate ~25% to head (initial startup logs, command banner)
    // and ~70% to tail (fatal errors, stack traces, exit summaries).
    let head_limit = (max_chars * 25) / 100;
    let tail_limit = (max_chars * 70) / 100;

    let chars: Vec<char> = text.chars().collect();

    // Snap head to a newline if one exists in the upper half of the head budget
    let mut head_end = head_limit.min(char_count);
    if let Some(last_newline) = chars[..head_end].iter().rposition(|&c| c == '\n') {
        if last_newline >= head_limit / 2 {
            head_end = last_newline;
        }
    }

    // Snap tail to a newline if one exists in the first third of the tail budget
    let tail_start_raw = char_count.saturating_sub(tail_limit);
    let mut tail_start = tail_start_raw.max(head_end);
    if let Some(first_newline) = chars[tail_start..].iter().position(|&c| c == '\n') {
        if first_newline <= tail_limit / 3 {
            tail_start += first_newline + 1;
        }
    }

    if tail_start <= head_end {
        return text.to_string();
    }

    let head: String = chars[..head_end].iter().collect();
    let tail: String = chars[tail_start..].iter().collect();

    let omitted_chars = tail_start.saturating_sub(head_end);
    let omitted_lines = chars[head_end..tail_start]
        .iter()
        .filter(|&&c| c == '\n')
        .count();

    let line_str = if omitted_lines == 1 {
        "1 line".to_string()
    } else {
        format!("{omitted_lines} lines")
    };

    let head_trimmed = head.trim_end();
    let tail_trimmed = tail.trim_start();

    if head_trimmed.is_empty() {
        format!("[... {omitted_chars} characters ({line_str}) omitted ...]\n\n{tail_trimmed}")
    } else if tail_trimmed.is_empty() {
        format!("{head_trimmed}\n\n[... {omitted_chars} characters ({line_str}) omitted ...]")
    } else {
        format!("{head_trimmed}\n\n[... {omitted_chars} characters ({line_str}) omitted ...]\n\n{tail_trimmed}")
    }
}

/// Format one captured run for the model: exit code + stdout/stderr sections,
/// capped using head+tail sandwiching so critical compiler errors and test failure summaries
/// at the end of long runs are preserved. Shared by run_command and typecheck.
pub fn format_captured(output: std::process::Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let code = output
        .status
        .code()
        .map(|c| c.to_string())
        .unwrap_or_else(|| "terminated".into());
    let mut result = format!("exit code: {code}");
    if !stdout.trim().is_empty() {
        result.push_str("\n\n--- stdout ---\n");
        result.push_str(stdout.trim_end());
    }
    if !stderr.trim().is_empty() {
        result.push_str("\n\n--- stderr ---\n");
        result.push_str(stderr.trim_end());
    }
    truncate_head_tail(&result, MAX_FETCH_CHARS)
}



pub fn unified_diff(old: &str, new: &str) -> String {
    let diff = similar::TextDiff::from_lines(old, new);
    diff.unified_diff().to_string()
}

/// Prefix each line with its 1-based number and a tab, up to `max_chars` output characters.
/// Formats directly into a pre-allocated String with zero per-line allocations.
/// Returns (formatted_string, truncated_flag).
pub fn number_lines_capped(content: &str, max_chars: usize) -> (String, bool) {
    use std::fmt::Write;
    let mut out = String::with_capacity(content.len().min(max_chars.saturating_add(512)));
    let mut char_count = 0;
    let mut truncated = false;

    for (i, line) in content.lines().enumerate() {
        let line_num = i + 1;
        let start_len = out.len();
        let _ = write!(out, "{line_num}\t{line}\n");
        let added_chars = out[start_len..].chars().count();
        char_count += added_chars;
        if char_count > max_chars {
            let mut c = 0;
            let mut byte_idx = out.len();
            for (idx, _) in out.char_indices() {
                if c == max_chars {
                    byte_idx = idx;
                    break;
                }
                c += 1;
            }
            out.truncate(byte_idx);
            truncated = true;
            break;
        }
    }
    (out, truncated)
}

/// Prefix each line with its 1-based number and a tab. Uses `str::lines()` (no trailing empty line).
#[allow(dead_code)]
pub fn number_lines(content: &str) -> String {
    number_lines_capped(content, usize::MAX).0
}

/// Splice `new_text` over the 1-based inclusive line range [start, end] of
/// `current` (end = start-1 inserts before start; empty new_text deletes). Bounds
/// are the caller's responsibility. Preserves the file's line ending and whether
/// it had a trailing newline.
///
/// CRUCIAL: `new_text` is split with `lines()` — the SAME way the file is read —
/// so a trailing newline is a line *terminator*, not an extra blank line. Splitting
/// with `split('\n')` here was a bug: `"carve(...)\n"` became `["carve(...)", ""]`,
/// appending a stray blank line on every replacement whose new_text ended in a
/// newline (the natural way to write a line), which also threw off the model's own
/// line-number arithmetic on the following edit.
/// How long browser_open / browser_reload wait for a dev server to answer.


pub(crate) fn read_text_capped(path: &Path) -> Result<String, String> {
    const MAX_BYTES: u64 = 8 * 1024 * 1024;
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.len() > MAX_BYTES {
            return Err(format!(
                "it is {:.1} MB, too large to read whole (limit {} MB) — use grep_search to \
                 find what you need inside it, or read_file_lines for a range",
                meta.len() as f64 / 1_048_576.0,
                MAX_BYTES / 1_048_576
            ));
        }
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let content = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        // Strip UTF-8 BOM
        String::from_utf8(bytes[3..].to_vec()).map_err(|e| e.to_string())?
    } else if bytes.starts_with(&[0xFF, 0xFE]) {
        // UTF-16 LE BOM
        let u16_chars: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16(&u16_chars).map_err(|e| format!("invalid UTF-16 LE: {e}"))?
    } else if bytes.starts_with(&[0xFE, 0xFF]) {
        // UTF-16 BE BOM
        let u16_chars: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16(&u16_chars).map_err(|e| format!("invalid UTF-16 BE: {e}"))?
    } else {
        match String::from_utf8(bytes) {
            Ok(s) => s,
            Err(e) => return Err(e.to_string()),
        }
    };
    Ok(content.strip_prefix('\u{feff}').unwrap_or(&content).to_string())
}



#[cfg(test)]
mod read_text_capped_tests {
    use super::read_text_capped;
    use std::path::{Path, PathBuf};

    struct TestFile(PathBuf);
    impl TestFile {
        fn new(name: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "outpost_read_f_{name}_{}_{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            let _ = std::fs::remove_file(&p);
            TestFile(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TestFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn strips_utf8_bom() {
        let temp = TestFile::new("utf8_bom");
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(b"hello world\nsecond line");
        std::fs::write(temp.path(), &bytes).unwrap();

        let read = read_text_capped(temp.path()).unwrap();
        assert_eq!(read, "hello world\nsecond line");
        assert!(!read.starts_with('\u{feff}'));
    }

    #[test]
    fn decodes_utf16_le_with_bom() {
        let temp = TestFile::new("utf16_le");
        let s = "hello from utf16 le";
        let mut bytes = vec![0xFF, 0xFE];
        for b in s.encode_utf16() {
            bytes.extend_from_slice(&b.to_le_bytes());
        }
        std::fs::write(temp.path(), &bytes).unwrap();

        let read = read_text_capped(temp.path()).unwrap();
        assert_eq!(read, "hello from utf16 le");
    }

    #[test]
    fn decodes_utf16_be_with_bom() {
        let temp = TestFile::new("utf16_be");
        let s = "hello from utf16 be";
        let mut bytes = vec![0xFE, 0xFF];
        for b in s.encode_utf16() {
            bytes.extend_from_slice(&b.to_be_bytes());
        }
        std::fs::write(temp.path(), &bytes).unwrap();

        let read = read_text_capped(temp.path()).unwrap();
        assert_eq!(read, "hello from utf16 be");
    }
}

#[cfg(test)]


#[cfg(test)]
mod truncate_head_tail_tests {
    use super::*;

    #[test]
    fn truncate_head_tail_returns_short_text_unchanged() {
        let short = "exit code: 0\n\n--- stdout ---\nall tests passed";
        assert_eq!(truncate_head_tail(short, 1000), short);
    }

    #[test]
    fn truncate_head_tail_preserves_head_and_tail_with_marker() {
        let mut lines = Vec::new();
        for i in 1..=400 {
            lines.push(format!("log line #{i:03}: compiling module_{i}"));
        }
        let full_text = lines.join("\n");
        let result = truncate_head_tail(&full_text, 2000);

        // Should contain beginning
        assert!(result.contains("log line #001: compiling module_1"), "should keep head");
        // Should contain end
        assert!(result.contains("log line #400: compiling module_400"), "should keep tail");
        // Should omit middle
        assert!(!result.contains("log line #200: compiling module_200"), "should omit middle");
        // Should have clear omission marker
        assert!(result.contains("omitted"), "should report omission: {result}");
        assert!(result.contains("lines) omitted"), "should report omitted lines: {result}");
    }

    #[test]
    fn truncate_head_tail_preserves_end_compiler_error() {
        // Simulates a build that emits thousands of characters of noise before failing at the end
        let mut output = String::from("exit code: 101\n\n--- stdout ---\n");
        for i in 1..=200 {
            output.push_str(&format!("test test_{i} ... ok\n"));
        }
        output.push_str("\n--- stderr ---\n");
        output.push_str("failures:\n\n---- test_auth_token stdout ----\n");
        output.push_str("thread 'test_auth_token' panicked at 'assertion failed: `(left == right)`'\n");
        output.push_str("  left: `401`,\n right: `200`\n\n");
        output.push_str("failures:\n    test_auth_token\n\n");
        output.push_str("test result: FAILED. 199 passed; 1 failed; finished in 2.34s\n");

        let truncated = truncate_head_tail(&output, 3000);

        // Head must be preserved (exit code and start of run)
        assert!(truncated.starts_with("exit code: 101"), "head preserved");
        // Tail error message and failure summary must be preserved
        assert!(truncated.contains("test_auth_token stdout"), "failure detail preserved");
        assert!(truncated.contains("thread 'test_auth_token' panicked"), "panic detail preserved");
        assert!(truncated.contains("FAILED. 199 passed; 1 failed"), "final summary preserved");
        // Middle passing tests should be omitted
        assert!(!truncated.contains("test test_100 ... ok"), "middle noise omitted");
    }

    #[test]
    fn truncate_head_tail_handles_no_newlines_and_edge_cases() {
        assert_eq!(truncate_head_tail("", 100), "");
        assert_eq!(truncate_head_tail("hello", 0), "");

        let unbroken = "x".repeat(5000);
        let result = truncate_head_tail(&unbroken, 1000);
        assert!(result.contains("omitted"));
        assert!(result.len() < 5000);
    }


}
