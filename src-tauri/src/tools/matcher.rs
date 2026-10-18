// ── surface: backend ──────────────────────────────────
// Fuzzy snippet edit engine & Tree-Sitter symbol replacement.
// Exports: resolve_snippet_edit · resolve_symbol_edit · rewrite_tool_for · SnippetMatch

use crate::tools::symbols;

/// Rewrite `s` to use whatever line endings `file` already uses.
///
/// Models routinely emit LF even when the on-disk file is Windows CRLF, and that
/// is the most common cause of "old_string was not found" — and the most
/// confusing, because it's intermittent: model-AUTHORED files are LF, so the same
/// edit works on some files and not others. Both the needle and the replacement
/// go through this, so the match succeeds AND the insert doesn't leave the file
/// with mixed endings. Existing endings are never rewritten.
pub fn match_file_endings(file: &str, s: &str) -> String {
    let lf = s.replace("\r\n", "\n");
    if file.contains("\r\n") {
        lf.replace('\n', "\r\n")
    } else {
        lf
    }
}

pub fn splice_lines(current: &str, start: usize, end: usize, new_text: &str) -> String {
    let ending = if current.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let trailing = current.ends_with('\n');
    let orig: Vec<&str> = current.lines().collect();
    let normalized = new_text.replace("\r\n", "\n");
    let new_lines: Vec<&str> = normalized.lines().collect();

    let mut result: Vec<&str> = Vec::new();
    result.extend_from_slice(&orig[..start - 1]);
    result.extend(new_lines.iter().copied());
    result.extend_from_slice(&orig[end..]);
    let mut updated = result.join(ending);
    if trailing && !updated.is_empty() {
        updated.push_str(ending);
    }
    updated
}

/// Adjust `new_text` indentation to harmonize with the target lines in `current`.
/// When the model provides `old_string` and `new_string` without the enclosing indentation
/// (e.g. 0 spaces or 2 spaces when the file has 4 or 8 spaces), this shifts `new_string`
/// lines proportionally so the replacement blends seamlessly into the file's layout.
pub fn harmonize_snippet_indent(
    orig_lines: &[&str],
    old_text: &str,
    new_text: &str,
) -> String {
    // 1. Find target indentation from first non-blank line in orig_lines
    let target_line = match orig_lines.iter().find(|l| !l.trim().is_empty()) {
        Some(l) => *l,
        None => return new_text.to_string(),
    };
    let target_indent_len = target_line.len() - target_line.trim_start().len();
    let target_indent = &target_line[..target_indent_len];

    // 2. Find base indentation of old_text from first non-blank line
    let old_line = match old_text.lines().find(|l| !l.trim().is_empty()) {
        Some(l) => l,
        None => return new_text.to_string(),
    };
    let old_indent_len = old_line.len() - old_line.trim_start().len();
    let old_indent = &old_line[..old_indent_len];

    // 3. Find base indentation of new_text from first non-blank line
    let new_line = match new_text.lines().find(|l| !l.trim().is_empty()) {
        Some(l) => l,
        None => return new_text.to_string(),
    };
    let new_first_indent_len = new_line.len() - new_line.trim_start().len();
    let new_first_indent = &new_line[..new_first_indent_len];

    // If new_text already matches the target indentation, no adjustment needed
    if new_first_indent == target_indent {
        return new_text.to_string();
    }

    // If old_text already had the same indentation as target_indent, the model's new_text
    // indentation was intentional (e.g. intentionally re-indenting or un-nesting a block).
    if old_indent == target_indent {
        return new_text.to_string();
    }

    let ending = if new_text.contains("\r\n") { "\r\n" } else { "\n" };
    let trailing_nl = new_text.ends_with('\n');

    let lines: Vec<&str> = new_text.lines().collect();
    let mut adjusted_lines = Vec::with_capacity(lines.len());

    // Case A: target indentation is deeper than old_indent (e.g. 0/2 spaces in old, 4/8 spaces in target)
    if target_indent.starts_with(old_indent) && target_indent.len() > old_indent.len() {
        let prefix = &target_indent[old_indent.len()..];
        for line in lines {
            if line.trim().is_empty() {
                adjusted_lines.push(String::new());
            } else {
                adjusted_lines.push(format!("{}{}", prefix, line));
            }
        }
    } else if old_indent.starts_with(target_indent) && old_indent.len() > target_indent.len() {
        // Case B: target indentation is shallower than old_indent
        let strip_len = old_indent.len() - target_indent.len();
        for line in lines {
            if line.trim().is_empty() {
                adjusted_lines.push(String::new());
            } else if line.len() >= strip_len
                && line.is_char_boundary(strip_len)
                && line[..strip_len].chars().all(|c| c.is_whitespace())
            {
                adjusted_lines.push(line[strip_len..].to_string());
            } else {
                adjusted_lines.push(line.to_string());
            }
        }
    } else {
        // Case C: indentation characters differ (e.g. tabs vs spaces)
        for line in lines {
            if line.trim().is_empty() {
                adjusted_lines.push(String::new());
            } else if line.starts_with(old_indent) {
                adjusted_lines.push(format!("{}{}", target_indent, &line[old_indent.len()..]));
            } else {
                adjusted_lines.push(line.to_string());
            }
        }
    }

    let mut result = adjusted_lines.join(ending);
    if trailing_nl && !result.is_empty() {
        result.push_str(ending);
    }
    result
}

/// Whitespace tokens of a string — the unit whitespace-insensitive matching
/// works in. Tabs, spaces, CRLF vs LF and blank lines all vanish; token ORDER
/// and CONTIGUITY survive, which is what keeps the match honest.
fn ws_tokens(s: &str) -> Vec<&str> {
    s.split_whitespace().collect()
}

/// Every byte span in `haystack` whose whitespace-token sequence equals
/// `needle`'s. A span runs from the first token's start to the LAST token's
/// end, so a needle with blank lines inside can match a region wider than its
/// own character width — that wider region is what the replacement covers.
/// Empty needle matches nothing; callers decide on uniqueness.
fn fuzzy_spans(needle: &str, haystack: &str) -> Vec<(usize, usize)> {
    let needle_tokens = ws_tokens(needle);
    let hay_tokens = ws_tokens(haystack);
    if needle_tokens.is_empty() || hay_tokens.len() < needle_tokens.len() {
        return Vec::new();
    }
    let base = haystack.as_ptr() as usize;
    let mut spans = Vec::new();
    for start in 0..=(hay_tokens.len() - needle_tokens.len()) {
        if hay_tokens[start..start + needle_tokens.len()] == needle_tokens[..] {
            let first = hay_tokens[start];
            let last = hay_tokens[start + needle_tokens.len() - 1];
            let s = first.as_ptr() as usize - base;
            let e = last.as_ptr() as usize - base + last.len();
            spans.push((s, e));
        }
    }
    spans
}

/// How a snippet edit resolved against the file's current content.
#[derive(Debug, PartialEq, Eq)]
pub enum SnippetMatch {
    /// One exact, byte-identical occurrence — the strict path.
    Exact,
    /// No exact hit; matched via whitespace/indentation/comment normalization (1-based line span).
    Fuzzy { start_line: usize, end_line: usize },
    /// Matched via top/bottom anchor or sliding-window line similarity (1-based line span).
    Approximate { start_line: usize, end_line: usize },
    /// Too many matches to pick safely (exact or fuzzy).
    Ambiguous(usize),
    /// Nothing matched, exactly or fuzzily.
    NotFound(Option<String>),
}

/// Resolve a `project_edit`-style snippet replacement against `current`:
/// exact match first (CRLF-tolerant via match_file_endings), then the
/// whitespace-insensitive fallback. Returns the updated content (meaningful
/// for Exact/Fuzzy) plus how it matched; ambiguity and not-found are phrased
/// by each caller so the error can name the right tool.

fn closest_match_hint(old: &str, current: &str) -> Option<String> {
    let old_lines: Vec<&str> = old.lines().filter(|l| !l.trim().is_empty()).collect();
    if old_lines.is_empty() { return None; }
    
    let norm_old: Vec<String> = old_lines.iter().map(|l| l.trim().to_string()).collect();
    let cur_lines: Vec<&str> = current.lines().collect();
    if cur_lines.is_empty() { return None; }
    
    let mut best_score = 0;
    let mut best_idx = 0;
    
    let window_size = old_lines.len();
    if cur_lines.len() < window_size { return None; }
    
    for i in 0..=(cur_lines.len() - window_size) {
        let mut score = 0;
        for j in 0..window_size {
            if cur_lines[i+j].trim() == norm_old[j] {
                score += 1;
            }
        }
        if score > best_score {
            best_score = score;
            best_idx = i;
        }
    }
    
    if best_score > 0 && (best_score as f32 / window_size as f32 >= 0.4 || best_score >= 2 || window_size == 1) {
        let end_idx = (best_idx + window_size + 1).min(cur_lines.len());
        let start_idx = best_idx.saturating_sub(1);
        
        // Add line numbers to the snippet
        let mut snippet = String::new();
        for (i, line) in cur_lines[start_idx..end_idx].iter().enumerate() {
            snippet.push_str(&format!("{} | {}\n", start_idx + i + 1, line));
        }
        
        return Some(format!("Did you mean this block near line {}?\n{}", start_idx + 1, snippet.trim_end()));
    }
    None
}

fn apply_boundary_replacement(current: &str, start: usize, end: usize, new: &str) -> String {
    // Check leading boundary
    let line_start = current[..start].rfind('\n').map(|p| p + 1).unwrap_or(0);
    let leading = &current[line_start..start];
    let is_line_start = leading.chars().all(|c| c.is_whitespace());

    let actual_start = if is_line_start && new.chars().next().map_or(false, |c| c.is_whitespace()) {
        line_start
    } else {
        start
    };

    // Check trailing boundary
    let rest = &current[end..];
    let next_nl = rest.find('\n');
    let trailing_to_nl = match next_nl {
        Some(nl) => &rest[..nl],
        None => rest,
    };
    let is_line_end = trailing_to_nl.chars().all(|c| c.is_whitespace());

    let actual_end = if is_line_end && new.ends_with('\n') {
        if let Some(nl) = next_nl {
            end + nl + 1
        } else {
            end + trailing_to_nl.len()
        }
    } else {
        end
    };

    let mut out = String::with_capacity(current.len() + new.len());
    out.push_str(&current[..actual_start]);
    out.push_str(new);
    out.push_str(&current[actual_end..]);
    out
}

fn is_comment_or_blank(line: &str) -> bool {
    let t = line.trim();
    t.is_empty()
        || t.starts_with("//")
        || t.starts_with("/*")
        || t.starts_with('*')
        || t.starts_with("*/")
        || t.starts_with('#')
        || t.starts_with("--")
        || t.starts_with("<!--")
}

fn is_ellipsis_line(line: &str) -> bool {
    let t = line.trim();
    t == "..."
        || t == "…"
        || t.starts_with("// ...")
        || t.starts_with("//...")
        || t.starts_with("/* ...")
        || t.starts_with("# ...")
        || t.starts_with("#...")
        || t.contains("existing code")
        || t.contains("rest of code")
}

/// Match multi-line snippets ignoring intermediate comments and blank lines that the model omitted.
fn match_ignoring_comments(
    cur_trimmed: &[&str],
    target_lines: &[&str],
) -> Option<Result<(usize, usize), usize>> {
    let target_code: Vec<&str> = target_lines
        .iter()
        .copied()
        .filter(|l| !is_comment_or_blank(l))
        .collect();

    if target_code.len() < 2 {
        return None;
    }

    let mut matches = Vec::new();
    let n = cur_trimmed.len();

    for i in 0..n {
        if is_comment_or_blank(cur_trimmed[i]) {
            continue;
        }
        if cur_trimmed[i] != target_code[0] {
            continue;
        }

        let mut target_idx = 1;
        let mut cur_idx = i + 1;
        while cur_idx < n && target_idx < target_code.len() {
            if is_comment_or_blank(cur_trimmed[cur_idx]) {
                cur_idx += 1;
                continue;
            }
            if cur_trimmed[cur_idx] == target_code[target_idx] {
                target_idx += 1;
                if target_idx == target_code.len() {
                    break;
                }
            } else {
                break;
            }
            cur_idx += 1;
        }

        if target_idx == target_code.len() {
            matches.push((i, cur_idx));
        }
    }

    if matches.len() == 1 {
        Some(Ok(matches[0]))
    } else if matches.len() > 1 {
        Some(Err(matches.len()))
    } else {
        None
    }
}

/// Match by Top Anchor and Bottom Anchor when the model explicitly elides middle lines with `...`.
fn match_top_bottom_anchors(
    cur_trimmed: &[&str],
    target_lines: &[&str],
) -> Option<Result<(usize, usize), usize>> {
    let non_blank: Vec<&str> = target_lines
        .iter()
        .copied()
        .filter(|l| !l.is_empty())
        .collect();

    if non_blank.len() < 2 {
        return None;
    }

    let has_ellipsis = non_blank.iter().any(|l| is_ellipsis_line(l));
    if !has_ellipsis {
        return None;
    }
    let first = non_blank.first().copied().unwrap();
    let last = non_blank.last().copied().unwrap();

    // If first and last are identical or both are trivial punctuation, don't anchor
    if first == last || (first.len() < 3 && last.len() < 3) {
        return None;
    }

    let is_strong_anchor = |s: &str| s.chars().any(|c| c.is_alphanumeric());

    // Head anchor: take first line, or first 2 lines if line 0 is punctuation/short
    let (head_1, head_2) = if !is_strong_anchor(first) && non_blank.len() >= 3 {
        (non_blank[0], Some(non_blank[1]))
    } else {
        (first, None)
    };

    // Tail anchor: take last line, or last 2 lines if last line is punctuation/short
    let (tail_prev, tail_last) = if !is_strong_anchor(last) && non_blank.len() >= 3 {
        (Some(non_blank[non_blank.len() - 2]), last)
    } else {
        (None, last)
    };

    let max_span = target_lines.len() + 35;
    let mut matches = Vec::new();

    for i in 0..cur_trimmed.len() {
        if cur_trimmed[i] != head_1 {
            continue;
        }
        if let Some(h2) = head_2 {
            if i + 1 >= cur_trimmed.len() || cur_trimmed[i + 1] != h2 {
                continue;
            }
        }

        let start_pos = i;
        let search_start = if head_2.is_some() { i + 2 } else { i + 1 };
        let search_end = (start_pos + max_span).min(cur_trimmed.len());

        for j in search_start..search_end {
            if cur_trimmed[j] != tail_last {
                continue;
            }
            if let Some(tp) = tail_prev {
                if j == 0 || cur_trimmed[j - 1] != tp {
                    continue;
                }
            }

            matches.push((start_pos, j));
        }
    }

    if matches.len() == 1 {
        Some(Ok(matches[0]))
    } else if matches.len() > 1 {
        Some(Err(matches.len()))
    } else {
        None
    }
}

/// Match by sliding window line similarity (>= 80% line agreement) when a model has a minor typo or omitted line.
///
/// Safety invariant: the first and last lines of `target_lines` MUST match the window boundaries
/// (`cur_trimmed[i] == target_lines[0]` and `cur_trimmed[i + w - 1] == target_lines[w - 1]`).
/// Without boundary anchoring, an 80% match where the leading or trailing lines of `old_string`
/// are wrong would silently overwrite adjacent lines outside the intended edit span.
fn match_sliding_window(
    cur_trimmed: &[&str],
    target_lines: &[&str],
) -> Option<Result<(usize, usize), usize>> {
    let w = target_lines.len();
    if w < 4 || cur_trimmed.len() < w {
        return None;
    }

    let mut best_score = 0;
    let mut best_idx = 0;
    let mut second_best_score = 0;

    for i in 0..=(cur_trimmed.len() - w) {
        if cur_trimmed[i] != target_lines[0] || cur_trimmed[i + w - 1] != target_lines[w - 1] {
            continue;
        }
        let mut score = 0;
        for j in 0..w {
            if cur_trimmed[i + j] == target_lines[j] {
                score += 1;
            }
        }
        if score > best_score {
            second_best_score = best_score;
            best_score = score;
            best_idx = i;
        } else if score > second_best_score {
            second_best_score = score;
        }
    }

    let ratio = best_score as f32 / w as f32;
    let second_ratio = second_best_score as f32 / w as f32;
    // Require at least 80% match, and clear separation from second best match
    if ratio >= 0.80 && (best_score > second_best_score + 1 || second_ratio < 0.5) {
        Some(Ok((best_idx, best_idx + w - 1)))
    } else {
        None
    }
}

pub fn resolve_snippet_edit(current: &str, old: &str, new: &str) -> (String, SnippetMatch) {
    let old = match_file_endings(current, old);
    let new = match_file_endings(current, new);

    // 1. Exact match (strict byte-for-byte, CRLF-normalized, with boundary alignment)
    let exact_matches: Vec<_> = current.match_indices(old.as_str()).collect();
    if exact_matches.len() == 1 {
        let (idx, matched_str) = exact_matches[0];
        let updated = apply_boundary_replacement(current, idx, idx + matched_str.len(), &new);
        return (updated, SnippetMatch::Exact);
    }
    if exact_matches.len() > 1 {
        return (String::new(), SnippetMatch::Ambiguous(exact_matches.len()));
    }

    // 2. Line-trimmed match tier (for multi-line or single-line statements where indentation or surrounding blank lines differ)
    let cur_lines: Vec<&str> = current.lines().collect();
    let cur_trimmed: Vec<&str> = cur_lines.iter().map(|l| l.trim()).collect();
    let old_raw_lines: Vec<&str> = old.lines().map(|l| l.trim()).collect();

    let start_off = old_raw_lines.iter().position(|l| !l.is_empty()).unwrap_or(0);
    let end_off = old_raw_lines.iter().rposition(|l| !l.is_empty()).map(|p| p + 1).unwrap_or(old_raw_lines.len());
    let target_lines = if start_off < end_off {
        &old_raw_lines[start_off..end_off]
    } else {
        &old_raw_lines[..]
    };

    if !target_lines.is_empty() && cur_trimmed.len() >= target_lines.len() {
        let mut line_matches = Vec::new();
        for i in 0..=(cur_trimmed.len() - target_lines.len()) {
            if cur_trimmed[i..i + target_lines.len()] == *target_lines {
                line_matches.push(i);
            }
        }
        if line_matches.len() == 1 {
            let start_line = line_matches[0] + 1;
            let end_line = line_matches[0] + target_lines.len();
            let harmonized = harmonize_snippet_indent(&cur_lines[start_line - 1..end_line], &old, &new);
            let updated = splice_lines(current, start_line, end_line, &harmonized);
            return (updated, SnippetMatch::Fuzzy { start_line, end_line });
        } else if line_matches.len() > 1 {
            return (String::new(), SnippetMatch::Ambiguous(line_matches.len()));
        }
    }

    // 3. Comment-and-blank-line tolerant matching tier (handles omitted comments/blank lines in old_string)
    if let Some(res) = match_ignoring_comments(&cur_trimmed, target_lines) {
        match res {
            Ok((start_idx, end_idx)) => {
                let start_line = start_idx + 1;
                let end_line = end_idx + 1;
                let harmonized = harmonize_snippet_indent(&cur_lines[start_idx..=end_idx], &old, &new);
                let updated = splice_lines(current, start_line, end_line, &harmonized);
                return (updated, SnippetMatch::Fuzzy { start_line, end_line });
            }
            Err(n) => return (String::new(), SnippetMatch::Ambiguous(n)),
        }
    }

    // 4. Token-level fuzzy match (for mid-line edits, with smart boundary alignment)
    let spans = fuzzy_spans(old.as_str(), current);
    if spans.len() == 1 {
        let (s, e) = spans[0];
        let start_line = current[..s].matches('\n').count() + 1;
        let end_line = current[..e].matches('\n').count() + 1;
        let updated = apply_boundary_replacement(current, s, e, &new);
        return (updated, SnippetMatch::Fuzzy { start_line, end_line });
    } else if spans.len() > 1 {
        return (String::new(), SnippetMatch::Ambiguous(spans.len()));
    }

    // 5. Anchor matching tier (Top & Bottom Anchoring for explicit `...` ellipsis blocks)
    if let Some(res) = match_top_bottom_anchors(&cur_trimmed, target_lines) {
        match res {
            Ok((start_idx, end_idx)) => {
                let start_line = start_idx + 1;
                let end_line = end_idx + 1;
                let harmonized = harmonize_snippet_indent(&cur_lines[start_idx..=end_idx], &old, &new);
                let updated = splice_lines(current, start_line, end_line, &harmonized);
                return (updated, SnippetMatch::Approximate { start_line, end_line });
            }
            Err(n) => return (String::new(), SnippetMatch::Ambiguous(n)),
        }
    }

    // 6. Sliding-window line similarity (for near-exact matches with minor typos/discrepancies)
    if let Some(res) = match_sliding_window(&cur_trimmed, target_lines) {
        match res {
            Ok((start_idx, end_idx)) => {
                let start_line = start_idx + 1;
                let end_line = end_idx + 1;
                let harmonized = harmonize_snippet_indent(&cur_lines[start_idx..=end_idx], &old, &new);
                let updated = splice_lines(current, start_line, end_line, &harmonized);
                return (updated, SnippetMatch::Approximate { start_line, end_line });
            }
            Err(n) => return (String::new(), SnippetMatch::Ambiguous(n)),
        }
    }

    (String::new(), SnippetMatch::NotFound(closest_match_hint(&old, current)))
}

/// Adjust indentation of a replacement symbol definition to align with the target site's indentation.
pub fn align_symbol_replacement_indent(target_indent: &str, new_text: &str) -> String {
    let new_line = match new_text.lines().find(|l| !l.trim().is_empty()) {
        Some(l) => l,
        None => return new_text.to_string(),
    };
    let new_indent_len = new_line.len() - new_line.trim_start().len();
    let new_indent = &new_line[..new_indent_len];

    if new_indent == target_indent {
        return new_text.to_string();
    }

    let ending = if new_text.contains("\r\n") { "\r\n" } else { "\n" };
    let trailing_nl = new_text.ends_with('\n');
    let lines: Vec<&str> = new_text.lines().collect();
    let mut adjusted = Vec::with_capacity(lines.len());

    if target_indent.starts_with(new_indent) && target_indent.len() > new_indent.len() {
        let prefix = &target_indent[new_indent.len()..];
        for line in lines {
            if line.trim().is_empty() {
                adjusted.push(String::new());
            } else {
                adjusted.push(format!("{}{}", prefix, line));
            }
        }
    } else if new_indent.starts_with(target_indent) && new_indent.len() > target_indent.len() {
        let strip = new_indent.len() - target_indent.len();
        for line in lines {
            if line.trim().is_empty() {
                adjusted.push(String::new());
            } else if line.len() >= strip
                && line.is_char_boundary(strip)
                && line[..strip].chars().all(|c| c.is_whitespace())
            {
                adjusted.push(line[strip..].to_string());
            } else {
                adjusted.push(line.to_string());
            }
        }
    } else {
        for line in lines {
            if line.trim().is_empty() {
                adjusted.push(String::new());
            } else if line.starts_with(new_indent) {
                adjusted.push(format!("{}{}", target_indent, &line[new_indent.len()..]));
            } else {
                adjusted.push(line.to_string());
            }
        }
    }

    let mut result = adjusted.join(ending);
    if trailing_nl && !result.is_empty() {
        result.push_str(ending);
    }
    result
}

/// Resolve a symbol-scoped snippet edit (Superpower C) or full symbol replacement (Superpower D).
/// Returns Ok((updated_file_contents, notification_note)) or Err(user_facing_error_message).
pub fn resolve_symbol_edit(
    current: &str,
    ext: &str,
    symbol: &str,
    old: Option<&str>,
    new: &str,
) -> Result<(String, String), String> {
    let loc = symbols::find_symbol(current, ext, symbol)
        .map_err(|err| format!("error: {err}"))?;

    let old_str_opt = old.filter(|s| !s.is_empty());
    if let Some(old_str) = old_str_opt {
        // Superpower C: Symbol-Scoped Snippet Edit
        let scope_start = loc.outer_start_byte;
        let scope_end = loc.outer_end_byte;
        let scoped_content = &current[scope_start..scope_end];

        let (updated_scoped, matched) = resolve_snippet_edit(scoped_content, old_str, new);
        match matched {
            SnippetMatch::Exact | SnippetMatch::Fuzzy { .. } | SnippetMatch::Approximate { .. } => {
                let mut full_updated = String::with_capacity(current.len() + updated_scoped.len());
                full_updated.push_str(&current[..scope_start]);
                full_updated.push_str(&updated_scoped);
                full_updated.push_str(&current[scope_end..]);
                let matched_note = if matches!(matched, SnippetMatch::Approximate { .. }) {
                    format!(
                        " (scoped to symbol '{symbol}' at lines {}-{}, matched approximately via anchor/similarity fallback; indentation automatically harmonized)",
                        loc.start_line, loc.end_line
                    )
                } else if matches!(matched, SnippetMatch::Fuzzy { .. }) {
                    format!(
                        " (scoped to symbol '{symbol}' at lines {}-{}, indentation automatically harmonized)",
                        loc.start_line, loc.end_line
                    )
                } else {
                    format!(
                        " (scoped to symbol '{symbol}' at lines {}-{})",
                        loc.start_line, loc.end_line
                    )
                };
                Ok((full_updated, matched_note))
            }
            SnippetMatch::Ambiguous(n) => Err(format!(
                "error: old_string appears {n} times inside symbol '{symbol}' (lines {}-{}) — include surrounding lines inside '{symbol}' so it matches uniquely.",
                loc.start_line, loc.end_line
            )),
            SnippetMatch::NotFound(hint) => {
                let mut msg = format!(
                    "error: old_string was not found inside symbol '{symbol}' (lines {}-{}). Check the symbol's current implementation and retry.",
                    loc.start_line, loc.end_line
                );
                if let Some(h) = hint {
                    msg.push_str(&format!("\n\nHint: {}", h));
                }
                Err(msg)
            }
        }
    } else {
        // Superpower D: Full Symbol Replacement
        let is_deletion = new.trim().is_empty();
        let replaces_outer = is_deletion
            || new.trim_start().starts_with('@')
            || new.trim_start().starts_with("export ");
        let raw_start = if replaces_outer {
            loc.outer_start_byte
        } else {
            loc.start_byte
        };
        let raw_end = if replaces_outer {
            loc.outer_end_byte
        } else {
            loc.end_byte
        };

        let start_line = if replaces_outer {
            loc.outer_start_line
        } else {
            loc.start_line
        };
        let end_line = if replaces_outer {
            loc.outer_end_line
        } else {
            loc.end_line
        };

        let line_start = current[..raw_start].rfind('\n').map(|p| p + 1).unwrap_or(0);
        let leading_ws = &current[line_start..raw_start];
        let target_start = if leading_ws.chars().all(|c| c.is_whitespace()) {
            line_start
        } else {
            raw_start
        };

        let target_slice = &current[target_start..raw_end];
        let orig_lines: Vec<&str> = target_slice.lines().collect();
        let _ = orig_lines;
        let harmonized_new = align_symbol_replacement_indent(leading_ws, new);

        let mut actual_end = raw_end;
        if harmonized_new.ends_with('\n') || is_deletion {
            if current[actual_end..].starts_with("\r\n") {
                actual_end += 2;
            } else if current[actual_end..].starts_with('\n') {
                actual_end += 1;
            }
        }

        let mut full_updated = String::with_capacity(current.len() + harmonized_new.len());
        full_updated.push_str(&current[..target_start]);
        full_updated.push_str(&harmonized_new);
        full_updated.push_str(&current[actual_end..]);

        let action = if is_deletion { "deleted" } else { "replaced" };
        let matched_note = format!(
            " ({action} symbol '{symbol}' at lines {start_line}-{end_line})",
        );
        Ok((full_updated, matched_note))
    }
}

/// The whole-file rewrite tool paired with an edit tool: 'md_edit' →
/// 'md_write', 'html_edit' → 'html_write'.
pub fn rewrite_tool_for(edit_tool: &str) -> String {
    format!("{}_write", edit_tool.trim_end_matches("_edit"))
}

/// Open a file with the OS default handler (a browser, for `.html`).
///
/// This used to build a `cmd /C start "" "<path>"` command line with `raw_arg`,
/// which meant a filename the MODEL chose was interpolated into a shell line: a
/// note named `report" & calc.exe & "x.html` closed the quote and ran a second
/// command. The write itself failed (Windows forbids `"` in a filename), but the
/// path had already been queued for opening, and this function never checked
/// that the file existed.
///
/// The fix is not better escaping — it is not using a shell. The opener plugin
/// passes the path as a real argument to the OS, so there is no command line for
/// a filename to break out of, on any platform.


#[cfg(test)]
mod splice_tests {
    use super::splice_lines;

    #[test]
    fn replace_one_line_trailing_newline_no_stray_blank() {
        // The reported bug: a 1-line replacement whose new_text ends in a newline
        // used to append a stray blank line. Must not anymore.
        assert_eq!(splice_lines("a\nb\nc\n", 2, 2, "B\n"), "a\nB\nc\n");
    }

    #[test]
    fn replace_one_line_no_trailing_newline() {
        assert_eq!(splice_lines("a\nb\nc\n", 2, 2, "B"), "a\nB\nc\n");
    }

    #[test]
    fn replace_with_multiple_lines() {
        assert_eq!(splice_lines("a\nb\nc\n", 2, 2, "X\nY\n"), "a\nX\nY\nc\n");
    }

    #[test]
    fn insert_before_line() {
        // end = start - 1 inserts without replacing.
        assert_eq!(splice_lines("a\nb\n", 2, 1, "NEW\n"), "a\nNEW\nb\n");
    }

    #[test]
    fn delete_lines() {
        assert_eq!(splice_lines("a\nb\nc\n", 2, 2, ""), "a\nc\n");
    }

    #[test]
    fn preserves_crlf() {
        assert_eq!(
            splice_lines("a\r\nb\r\nc\r\n", 2, 2, "B"),
            "a\r\nB\r\nc\r\n"
        );
    }

    #[test]
    fn no_trailing_newline_file_preserved() {
        assert_eq!(splice_lines("a\nb", 2, 2, "B"), "a\nB");
    }

    #[test]
    fn intentional_blank_line_kept() {
        // A genuinely-wanted blank line (two newlines) survives; only the single
        // terminator newline is dropped.
        assert_eq!(splice_lines("a\nb\n", 2, 2, "B\n\n"), "a\nB\n\n");
    }

}

#[cfg(test)]
mod edit_match_tests {
    use super::{fuzzy_spans, resolve_snippet_edit, rewrite_tool_for, SnippetMatch};

    #[test]
    fn fuzzy_finds_match_across_indentation_and_blank_lines() {
        let hay = "fn main() {\n    let x = 1;\n}\n";
        let spans = fuzzy_spans("let x = 1;", hay);
        assert_eq!(spans.len(), 1, "indented needle must still match");
        assert_eq!(&hay[spans[0].0..spans[0].1], "let x = 1;");
    }

    #[test]
    fn fuzzy_tolerates_crlf_and_spacing() {
        let hay = "a\r\nb\r\nc\r\n";
        assert_eq!(fuzzy_spans("a b c", hay).len(), 1);
        assert_eq!(fuzzy_spans("a  b   c", hay).len(), 1);
    }

    #[test]
    fn fuzzy_preserves_order_and_contiguity() {
        let hay = "alpha beta gamma";
        assert_eq!(
            fuzzy_spans("beta alpha", hay).len(),
            0,
            "reordered tokens must not match"
        );
        assert_eq!(
            fuzzy_spans("alpha gamma", hay).len(),
            0,
            "skipped tokens must not match"
        );
        assert_eq!(fuzzy_spans("gamma", hay).len(), 1);
    }

    #[test]
    fn fuzzy_blank_lines_widen_the_span() {
        let hay = "let a = 1;\n\nlet b = 2;\n";
        // A needle with an internal blank line matches the WIDER span including
        // the file's real blank line — that wider span is what gets replaced.
        let spans = fuzzy_spans("let a = 1; let b = 2;", hay);
        assert_eq!(spans.len(), 1);
        assert_eq!(&hay[spans[0].0..spans[0].1], "let a = 1;\n\nlet b = 2;");
    }

    #[test]
    fn fuzzy_reports_every_span_for_uniqueness_checks() {
        let hay = "x = 1\ny = 2\nx = 1\n";
        assert_eq!(fuzzy_spans("x = 1", hay).len(), 2);
        assert_eq!(fuzzy_spans("y = 2", hay).len(), 1);
        assert_eq!(
            fuzzy_spans("", hay).len(),
            0,
            "empty needle matches nothing"
        );
        assert_eq!(fuzzy_spans("z = 9", hay).len(), 0);
    }

    #[test]
    fn exact_match_wins_over_fuzzy() {
        let (updated, m) = resolve_snippet_edit("a\nB\nc\n", "B", "X");
        assert_eq!(updated, "a\nX\nc\n");
        assert!(matches!(m, SnippetMatch::Exact));
    }

    #[test]
    fn fuzzy_fallback_replaces_the_wider_actual_span() {
        let (updated, m) = resolve_snippet_edit("a\n    b\nc\n", "a\nb\nc", "A\nC");
        assert_eq!(updated, "A\nC\n");
        assert_eq!(m, SnippetMatch::Fuzzy { start_line: 1, end_line: 3 });
    }

    #[test]
    fn crlf_needle_matches_lf_file_and_back() {
        let (updated, m) = resolve_snippet_edit("a\r\nb\r\nc\r\n", "a\nb", "X");
        assert_eq!(updated, "X\r\nc\r\n");
        assert!(matches!(m, SnippetMatch::Exact)); // match_file_endings normalises first
    }

    #[test]
    fn not_found_and_ambiguous_are_reported() {
        assert!(matches!(
            resolve_snippet_edit("a b c", "x y z", "w").1,
            SnippetMatch::NotFound(_)
        ));
        assert!(matches!(
            resolve_snippet_edit("a b a b", "a b", "x").1,
            SnippetMatch::Ambiguous(_)
        ));
        // No exact hit, but two whitespace-insensitive hits — still ambiguous.
        assert!(matches!(
            resolve_snippet_edit("a b a b", "a  b", "x").1,
            SnippetMatch::Ambiguous(_)
        ));
    }

    #[test]
    fn notes_edit_falls_back_to_fuzzy_across_indentation() {
        // The md_edit/html_edit path resolves through the same helper as
        // project_edit — a model's old_string with different indentation than
        // the note still lands, replacing the wider actual span.
        let note = "# Title\n\n- item one\n  - nested\n- item three\n";
        let (updated, m) =
            resolve_snippet_edit(note, "- item one\n- nested", "- item ONE\n  - nested");
        assert!(matches!(m, SnippetMatch::Fuzzy { .. }));
        assert_eq!(updated, "# Title\n\n- item ONE\n  - nested\n- item three\n");
    }

    #[test]
    fn notes_edit_tolerates_internal_spacing() {
        // Extra spacing inside a line (HTML-ish content) is whitespace, so it
        // matches fuzzily; the replacement covers the whole actual span.
        let page = "<li>alpha beta</li>\n";
        let (updated, m) =
            resolve_snippet_edit(page, "<li>alpha  beta</li>", "<li>ALPHA beta</li>");
        assert!(matches!(m, SnippetMatch::Fuzzy { .. }));
        assert_eq!(updated, "<li>ALPHA beta</li>\n");
    }

    #[test]
    fn rewrite_tool_pairs_with_edit_tool() {
        assert_eq!(rewrite_tool_for("md_edit"), "md_write");
        assert_eq!(rewrite_tool_for("html_edit"), "html_write");
    }

    #[test]
    fn line_trimmed_replaces_multiline_with_different_indentation() {
        let cur = "fn main() {\n    let a = 1;\n    let b = 2;\n}\n";
        let old = "  let a = 1;\n  let b = 2;";
        let new = "    let a = 10;\n    let b = 20;";
        let (updated, m) = resolve_snippet_edit(cur, old, new);
        assert_eq!(updated, "fn main() {\n    let a = 10;\n    let b = 20;\n}\n");
        assert_eq!(m, SnippetMatch::Fuzzy { start_line: 2, end_line: 3 });
    }

    #[test]
    fn boundary_alignment_exact_avoids_doubling_indentation_and_newline() {
        let cur = "    let x = 1;\n";
        let old = "let x = 1;";
        let new = "    let x = 2;\n";
        let (updated, m) = resolve_snippet_edit(cur, old, new);
        assert_eq!(updated, "    let x = 2;\n");
        assert!(matches!(m, SnippetMatch::Exact));

        let cur2 = "fn foo() {\n    let x = 1;\n    let y = 2;\n}\n";
        let (updated2, m2) = resolve_snippet_edit(cur2, "let x = 1;", "    let x = 10;\n");
        assert_eq!(updated2, "fn foo() {\n    let x = 10;\n    let y = 2;\n}\n");
        assert!(matches!(m2, SnippetMatch::Exact));
    }

    #[test]
    fn boundary_alignment_fuzzy_avoids_doubling_indentation_and_newline() {
        let cur = "    let x = 1;\n";
        // Double space in old prevents exact match; falls back to fuzzy
        let old = "let  x  =  1;";
        let new = "    let x = 2;\n";
        let (updated, m) = resolve_snippet_edit(cur, old, new);
        assert_eq!(updated, "    let x = 2;\n");
        assert_eq!(m, SnippetMatch::Fuzzy { start_line: 1, end_line: 1 });

        let cur2 = "fn foo() {\n    let x = 1;\n    let y = 2;\n}\n";
        let (updated2, m2) = resolve_snippet_edit(cur2, "let  x  =  1;", "    let x = 10;\n");
        assert_eq!(updated2, "fn foo() {\n    let x = 10;\n    let y = 2;\n}\n");
        assert_eq!(m2, SnippetMatch::Fuzzy { start_line: 2, end_line: 2 });
    }

    #[test]
    fn line_trimmed_tolerates_accidental_outer_blank_lines() {
        let cur = "fn test() {\n    let val = 42;\n}\n";
        let old = "\n\n  let val = 42;\n\n";
        let new = "    let val = 99;";
        let (updated, m) = resolve_snippet_edit(cur, old, new);
        assert_eq!(updated, "fn test() {\n    let val = 99;\n}\n");
        assert_eq!(m, SnippetMatch::Fuzzy { start_line: 2, end_line: 2 });
    }

    #[test]
    fn comment_tolerant_matching_omitted_internal_comments() {
        let cur = "fn calculate() {\n    let x = 1;\n    // intermediate calculation comment\n    let y = 2;\n    x + y\n}\n";
        let old = "let x = 1;\nlet y = 2;\nx + y";
        let new = "let x = 10;\nlet y = 20;\nx + y";
        let (updated, m) = resolve_snippet_edit(cur, old, new);
        assert_eq!(m, SnippetMatch::Fuzzy { start_line: 2, end_line: 5 });
        // Harmonizer automatically indents the replacement to match the enclosing block
        assert_eq!(
            updated,
            "fn calculate() {\n    let x = 10;\n    let y = 20;\n    x + y\n}\n"
        );
    }

    #[test]
    fn anchor_matching_with_ellipsis() {
        let cur = "pub fn router() -> Router {\n    Router::new()\n        .route(\"/a\", get(a))\n        .route(\"/b\", get(b))\n        .route(\"/c\", get(c))\n}\n";
        let old = "pub fn router() -> Router {\n    // ... existing code ...\n    .route(\"/c\", get(c))\n}";
        let new = "pub fn router() -> Router {\n    Router::new()\n        .route(\"/all\", get(all))\n}";
        let (updated, m) = resolve_snippet_edit(cur, old, new);
        assert!(matches!(m, SnippetMatch::Approximate { start_line: 1, end_line: 6 }));
        assert_eq!(
            updated,
            "pub fn router() -> Router {\n    Router::new()\n        .route(\"/all\", get(all))\n}\n"
        );
    }

    #[test]
    fn sliding_window_recovers_from_minor_typo_in_middle() {
        let cur = "fn process() {\n    let step_one = 1;\n    let step_two = 2;\n    let step_three = 3;\n    let step_four = 4;\n    let step_five = 5;\n}\n";
        // Model wrote "step_tw = 2" instead of "step_two = 2"
        let old = "let step_one = 1;\nlet step_tw = 2;\nlet step_three = 3;\nlet step_four = 4;\nlet step_five = 5;";
        let new = "let step_all = 999;";
        let (updated, m) = resolve_snippet_edit(cur, old, new);
        assert!(matches!(m, SnippetMatch::Approximate { start_line: 2, end_line: 6 }));
        // Harmonizer automatically indents the replacement to match the enclosing block
        assert_eq!(
            updated,
            "fn process() {\n    let step_all = 999;\n}\n"
        );
    }

    #[test]
    fn sliding_window_refuses_mismatched_boundary_lines_to_prevent_clobbering_neighbors() {
        // 5 lines where lines 1..=4 match (80% similarity), but line 5 of old_string
        // is a guessed line that does NOT match line 5 of the file ("- **Undo.**").
        // Sliding window must refuse rather than clobbering line 5.
        let cur = "- **Step 1.** a\n- **Step 2.** b\n- **Step 3.** c\n- **Step 4.** d\n- **Undo.** keep me\n";
        let old = "- **Step 1.** a\n- **Step 2.** b\n- **Step 3.** c\n- **Step 4.** d\n- **Permissions.** wrong tail";
        let (_, m) = resolve_snippet_edit(cur, old, "replaced");
        assert!(
            matches!(m, SnippetMatch::NotFound(_)),
            "sliding window must not match when boundary lines differ"
        );
    }

    #[test]
    fn non_ellipsis_snippet_does_not_collapse_wide_span_via_top_bottom_anchors() {
        // Without an explicit `...` ellipsis, a 4-line snippet whose top and bottom lines
        // happen to match a 9-line block (with only 1 intermediate line in common) must NOT
        // collapse the 9-line block.
        let cur = "fn setup() {\n    let a = 1;\n    let b = 2;\n    let c = 3;\n    let d = 4;\n    let e = 5;\n    let f = 6;\n    let g = 7;\n}\n";
        let old = "fn setup() {\n    let a = 1;\n    let wrong = 99;\n}";
        let (_, m) = resolve_snippet_edit(cur, old, "fn setup() {}");
        assert!(
            matches!(m, SnippetMatch::NotFound(_)),
            "non-ellipsis snippet must not collapse a 9-line span: got {:?}",
            m
        );
    }

    #[test]
    fn harmonizes_multiline_python_class_method_indentation() {
        let cur = "class Service:\n    def execute(self):\n        start()\n        finish()\n";
        let old = "start()\nfinish()";
        let new = "start()\nif ready():\n    proceed()\nfinish()";
        let (updated, m) = resolve_snippet_edit(cur, old, new);
        assert_eq!(m, SnippetMatch::Fuzzy { start_line: 3, end_line: 4 });
        assert_eq!(
            updated,
            "class Service:\n    def execute(self):\n        start()\n        if ready():\n            proceed()\n        finish()\n"
        );
    }

    #[test]
    fn harmonizes_tab_indentation_for_go() {
        let cur = "package main\n\nfunc Run() {\n\tstep1()\n\tstep2()\n}\n";
        let old = "step1()\nstep2()";
        let new = "step1()\nstep_extra()\nstep2()";
        let (updated, m) = resolve_snippet_edit(cur, old, new);
        assert_eq!(m, SnippetMatch::Fuzzy { start_line: 4, end_line: 5 });
        assert_eq!(
            updated,
            "package main\n\nfunc Run() {\n\tstep1()\n\tstep_extra()\n\tstep2()\n}\n"
        );
    }

    #[test]
    fn preserves_intentional_unindent_when_old_indent_matches() {
        let cur = "fn main() {\n    if true {\n        do_work();\n    }\n}\n";
        let old = "    if true {\n        do_work();\n    }";
        let new = "    do_work();";
        let (updated, m) = resolve_snippet_edit(cur, old, new);
        assert!(matches!(m, SnippetMatch::Exact));
        assert_eq!(
            updated,
            "fn main() {\n    do_work();\n}\n"
        );
    }
}



#[cfg(test)]
mod symbol_edit_tests {
    use super::*;

    #[test]
    fn superpower_c_disambiguates_repeated_code_by_symbol() {
        let file = "def handle_a():\n    status = 200\n    return status\n\ndef handle_b():\n    status = 200\n    return status\n";
        // Plain snippet edit sees ambiguity (status = 200 appears twice)
        let (_, m) = resolve_snippet_edit(file, "status = 200", "status = 201");
        assert!(matches!(m, SnippetMatch::Ambiguous(2)));

        // Symbol-scoped edit restricts search to handle_b and succeeds uniquely
        let (updated, note) = resolve_symbol_edit(file, "py", "handle_b", Some("status = 200"), "status = 201").unwrap();
        assert!(note.contains("scoped to symbol 'handle_b'"));
        assert!(updated.contains("def handle_a():\n    status = 200"));
        assert!(updated.contains("def handle_b():\n    status = 201"));
    }

    #[test]
    fn superpower_d_full_symbol_replacement_without_old_string() {
        let file = "def calculate_tax(amount):\n    rate = 0.05\n    return amount * rate\n\ndef calculate_total(subtotal, tax):\n    return subtotal + tax\n";
        let new_fn = "def calculate_tax(amount):\n    rate = 0.07\n    return amount * rate";
        let (updated, note) = resolve_symbol_edit(file, "py", "calculate_tax", None, new_fn).unwrap();
        assert!(note.contains("replaced symbol 'calculate_tax'"));
        assert!(updated.contains("rate = 0.07"));
        assert!(updated.contains("def calculate_total(subtotal, tax):"));
    }

    #[test]
    fn superpower_d_harmonizes_indentation_of_replaced_method() {
        let file = "class PaymentGateway:\n    def process(self, card):\n        validate(card)\n        charge(card)\n";
        // Replacement is unindented (0 spaces base)
        let new_method = "def process(self, card):\n    validate(card)\n    audit(card)\n    charge(card)";
        let (updated, note) = resolve_symbol_edit(file, "py", "PaymentGateway.process", None, new_method).unwrap();
        assert!(note.contains("replaced symbol 'PaymentGateway.process'"));
        assert!(updated.contains("    def process(self, card):\n        validate(card)\n        audit(card)\n        charge(card)"));
    }

    #[test]
    fn superpower_c_rust_symbol_scoped_edit() {
        let file = "impl Server {\n    pub fn start(&self) {\n        let port = 8080;\n        listen(port);\n    }\n\n    pub fn ping(&self) {\n        let port = 8080;\n        ping(port);\n    }\n}\n";
        let (updated, note) = resolve_symbol_edit(file, "rs", "Server.start", Some("let port = 8080;"), "let port = 9000;").unwrap();
        assert!(note.contains("scoped to symbol 'Server.start'"));
        assert!(updated.contains("let port = 9000;"));
        assert!(updated.contains("pub fn ping(&self) {\n        let port = 8080;"));
    }

    #[test]
    fn symbol_not_found_returns_informative_error() {
        let file = "def foo(): pass\ndef bar(): pass\n";
        let err = resolve_symbol_edit(file, "py", "nonexistent", None, "def bar(): pass").unwrap_err();
        assert!(err.contains("symbol 'nonexistent' was not found"));
        assert!(err.contains("Available symbols: foo, bar"));
    }

    #[test]
    fn superpower_d_typescript_method_replacement() {
        let file = "class Cache {\n    get(key: string): string {\n        return this.store[key];\n    }\n}\n";
        let new_method = "    get(key: string): string {\n        this.stats.hits++;\n        return this.store[key];\n    }";
        let (updated, note) = resolve_symbol_edit(file, "ts", "Cache.get", None, new_method).unwrap();
        assert!(note.contains("replaced symbol 'Cache.get'"));
        assert!(updated.contains("this.stats.hits++;"));
    }

    #[test]
    fn superpower_d_symbol_deletion_removes_decorators() {
        let file = "@dataclass\nclass Snake:\n    x: int\n\nclass SnakeGame:\n    pass\n";
        let (updated, note) = resolve_symbol_edit(file, "py", "Snake", None, "").unwrap();
        assert!(note.contains("deleted symbol 'Snake' at lines 1-3"));
        assert!(!updated.contains("@dataclass"));
        assert!(!updated.contains("Snake:"));
        assert!(updated.contains("class SnakeGame:\n    pass\n"));
    }

    #[test]
    fn superpower_c_go_receiver_method_edit() {
        let file = "package main\n\nfunc (s *Service) Handle() {\n    status := 200\n    render(status)\n}\n";
        let (updated, note) = resolve_symbol_edit(file, "go", "Service.Handle", Some("status := 200"), "status := 201").unwrap();
        assert!(note.contains("scoped to symbol 'Service.Handle'"));
        assert!(updated.contains("status := 201"));
    }

    #[test]
    fn harmonize_snippet_indent_does_not_panic_on_multibyte_utf8() {
        // old_indent is 4 spaces, target_indent is 2 spaces -> strip_len is 2 bytes.
        // A line with 1 space + 3-byte character '—' (em-dash, byte len 3) has len 4 >= 2,
        // but byte 2 lands inside '—'. This must not panic.
        let snippet = "    first line\n — bullet\n    third line";
        let adjusted = harmonize_snippet_indent(&["  target"], "    ", snippet);
        assert!(adjusted.contains("first line"));
        assert!(adjusted.contains(" — bullet"));
    }
}
