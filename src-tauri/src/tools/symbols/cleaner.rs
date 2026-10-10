// ââ surface: backend ââââââââââââââââââââââââââââââââââ
// Signature cleaner and comment stripper for AST symbol extraction.
// Constrains symbols to 120-character display width with standardized ellipsis markers.

pub const SIG_WIDTH: usize = 120;

/// Strip comments from a code slice while respecting string and char literals.
pub fn strip_comments(raw: &str, ext: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let chars: Vec<char> = raw.chars().collect();
    let len = chars.len();
    let mut i = 0;
    let is_hash_comment_lang = matches!(ext, "py" | "gd");

    while i < len {
        let c = chars[i];
        let next = chars.get(i + 1).copied();

        // String literals: "...", '...', `...`
        if c == '"' || c == '\'' || c == '`' {
            let quote = c;
            out.push(quote);
            i += 1;
            while i < len {
                let sc = chars[i];
                out.push(sc);
                if sc == '\\' {
                    // Escaped character inside string
                    if i + 1 < len {
                        i += 1;
                        out.push(chars[i]);
                    }
                } else if sc == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }

        // Rust raw string literals: r#"..."#
        if (c == 'r' || c == 'b' || c == 'c') && next == Some('#') {
            let mut j = i + 1;
            let mut hashes = 0;
            while j < len && chars[j] == '#' {
                hashes += 1;
                j += 1;
            }
            if j < len && chars[j] == '"' {
                // Consume raw string up to '"' followed by same number of '#'
                for k in i..=j {
                    out.push(chars[k]);
                }
                i = j + 1;
                while i < len {
                    let rc = chars[i];
                    out.push(rc);
                    if rc == '"' {
                        let mut match_hashes = 0;
                        while match_hashes < hashes && (i + 1 + match_hashes) < len && chars[i + 1 + match_hashes] == '#' {
                            match_hashes += 1;
                        }
                        if match_hashes == hashes {
                            for _ in 0..hashes {
                                i += 1;
                                out.push(chars[i]);
                            }
                            i += 1;
                            break;
                        }
                    }
                    i += 1;
                }
                continue;
            }
        }

        // Line comment: // ...
        if c == '/' && next == Some('/') {
            // Skip until newline
            i += 2;
            while i < len && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }

        // Block comment: /* ... */
        if c == '/' && next == Some('*') {
            i += 2;
            let mut depth = 1;
            while i < len && depth > 0 {
                if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    i += 2;
                } else if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                    depth += 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            continue;
        }

        // Python / GDScript hash line comment: # ...
        if is_hash_comment_lang && c == '#' {
            i += 1;
            while i < len && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }

        out.push(c);
        i += 1;
    }

    out
}

/// Take at most `max_chars` Unicode characters from string.
pub fn take_chars(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

/// Clean a scanned declaration into a compact, single-line signature.
///
/// Two truncation markers, deliberately distinguishable:
///   * `...`   â the declaration is COMPLETE but wider than `SIG_WIDTH`, so the
///               marker is display truncation and nothing is structurally missing.
///   * `, ...` â the declaration is INCOMPLETE: reading stopped before the header
///               closed, so parameters past this point were never seen.
///
/// Plus `= ...` on a `const`, which is a third distinct signal: the declaration is
/// complete, but a value was dropped and no type annotation stood in for it.
pub fn clean_sig(raw: &str, complete: bool) -> String {
    let trimmed = raw.trim();
    let unbraced = trimmed
        .trim_end_matches('{')
        .trim_end_matches(':')
        .trim_end_matches(';')
        .trim();
    // A dangling `=` left over from a multi-line body (`export type P = {`).
    let unbraced = unbraced.trim_end_matches('=').trim();
    // Collapse internal whitespace runs
    let words: Vec<&str> = unbraced.split_whitespace().collect();
    let mut s = words.join(" ");
    s = s
        .replace("( ", "(")
        .replace(", )", ")")
        .replace(",)", ")")
        .replace(" )", ")")
        .replace(" ,", ",")
        .replace(", >", ">");

    if complete {
        if s.chars().count() <= SIG_WIDTH {
            return s;
        }
        // Back off any trailing comma, so a display-truncated *complete* signature
        // cannot end up looking like the `, ...` incomplete marker below.
        let cut = take_chars(&s, SIG_WIDTH - 3);
        let cut = cut.trim_end().trim_end_matches(',').trim_end();
        return format!("{cut}...");
    }

    // Incomplete â mark it, and cut on a parameter boundary where possible.
    let mut body = s.trim_end_matches(',').trim().to_string();
    if body.chars().count() > SIG_WIDTH - 5 {
        let cut = take_chars(&body, SIG_WIDTH - 5);
        body = match cut.rfind(',') {
            Some(i) if i > cut.len() / 2 => cut[..i].to_string(),
            _ => cut,
        };
    }
    let body = body.trim_end_matches(',').trim();
    if body.ends_with('(') || body.ends_with('{') {
        format!("{body}...")
    } else {
        format!("{body}, ...")
    }
}
