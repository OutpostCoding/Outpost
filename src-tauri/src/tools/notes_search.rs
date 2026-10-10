// ── surface: backend ──────────────────────────────────
// Relevance-ranked note & page search for the agentic library (spec §5).
// BM25 + filename/title boosting + exact-phrase priority + contextual windowed snippets.
// Exports: rank_and_snippet, NoteMatch · Related: tools/mod.rs (search_files)

use std::collections::{HashMap, HashSet};

/// BM25 free parameters (Okapi defaults, matching code_search.rs).
const K1: f64 = 1.2;
const B: f64 = 0.75;

/// Max characters for an individual snippet line before smart windowing centers around the match.
const MAX_SNIPPET_LINE_CHARS: usize = 140;

/// Max snippet lines shown per matching note.
const MAX_SNIPPETS_PER_NOTE: usize = 2;

/// A ranked note match ready for display.
#[derive(Debug, Clone, PartialEq)]
pub struct NoteMatch {
    pub path: String,
    pub score: f64,
    pub snippets: Vec<String>,
}

/// Tokenize text into lowercase alphanumeric tokens.
pub fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    for c in text.chars() {
        if c.is_alphanumeric() {
            cur.push(c.to_ascii_lowercase());
        } else if !cur.is_empty() {
            tokens.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }
    tokens
}

/// Extract the primary title from markdown (# Heading) or HTML (<title> or <h1>).
pub fn extract_title(path: &str, content: &str) -> Option<String> {
    let p_lower = path.to_lowercase();
    if p_lower.ends_with(".md") {
        for line in content.lines().take(20) {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("# ") {
                let t = rest.trim();
                if !t.is_empty() {
                    return Some(t.to_string());
                }
            }
        }
    } else if p_lower.ends_with(".html") || p_lower.ends_with(".htm") {
        let lower = content.to_lowercase();
        if let Some(start) = lower.find("<title>") {
            if let Some(end) = lower[start + 7..].find("</title>") {
                let t = content[start + 7..start + 7 + end].trim();
                if !t.is_empty() {
                    return Some(t.to_string());
                }
            }
        }
        if let Some(start) = lower.find("<h1>") {
            if let Some(end) = lower[start + 4..].find("</h1>") {
                let t = content[start + 4..start + 4 + end].trim();
                if !t.is_empty() {
                    return Some(t.to_string());
                }
            }
        }
    }
    None
}

/// Extract the filename stem without extension (e.g. "recipes/curry.md" -> "curry").
pub fn extract_stem(path: &str) -> String {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    if let Some(idx) = name.rfind('.') {
        name[..idx].to_string()
    } else {
        name.to_string()
    }
}

/// Smart windowing: centers a snippet around the matched term if the line is long.
pub fn window_snippet(line: &str, query_tokens: &[String], exact_phrase: &str) -> String {
    let trimmed = line.trim();
    if trimmed.chars().count() <= MAX_SNIPPET_LINE_CHARS {
        return trimmed.to_string();
    }

    let line_lc = trimmed.to_lowercase();

    // Prefer locating the exact phrase match if available, else first matching query token.
    let match_byte_pos = if !exact_phrase.is_empty() {
        line_lc.find(exact_phrase)
    } else {
        None
    }
    .or_else(|| {
        query_tokens
            .iter()
            .filter_map(|t| line_lc.find(t.as_str()))
            .min()
    });

    let Some(byte_pos) = match_byte_pos else {
        let mut s: String = trimmed.chars().take(MAX_SNIPPET_LINE_CHARS).collect();
        s.push('…');
        return s;
    };

    let char_pos = trimmed[..byte_pos].chars().count();
    let total_chars = trimmed.chars().count();

    let half = MAX_SNIPPET_LINE_CHARS / 2;
    let start_char = char_pos.saturating_sub(half);
    let end_char = (start_char + MAX_SNIPPET_LINE_CHARS).min(total_chars);
    let start_char = end_char.saturating_sub(MAX_SNIPPET_LINE_CHARS);

    let prefix = if start_char > 0 { "…" } else { "" };
    let suffix = if end_char < total_chars { "…" } else { "" };

    let slice: String = trimmed.chars().skip(start_char).take(end_char - start_char).collect();
    format!("{prefix}{slice}{suffix}")
}

/// Extract up to `MAX_SNIPPETS_PER_NOTE` best lines in `content` with 1-based line numbers.
pub fn extract_snippets(
    content: &str,
    title: Option<&str>,
    query_tokens: &[String],
    exact_phrase: &str,
) -> Vec<String> {
    let exact_lc = exact_phrase.to_lowercase();
    let token_set: HashSet<&str> = query_tokens.iter().map(String::as_str).collect();

    let mut scored_lines: Vec<(usize, usize, String)> = Vec::new();

    for (i, raw_line) in content.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let line_lc = line.to_lowercase();

        let mut line_score = 0;

        if !exact_lc.is_empty() && line_lc.contains(&exact_lc) {
            line_score += 25;
        }

        let line_tokens = tokenize(line);
        let mut distinct_matches = 0;
        for t in &line_tokens {
            if token_set.contains(t.as_str()) {
                distinct_matches += 1;
            }
        }
        line_score += distinct_matches * 5;

        if line.starts_with('#') {
            line_score += 3;
        }

        if line_score > 0 {
            let snip = window_snippet(line, query_tokens, &exact_lc);
            scored_lines.push((line_score, i + 1, snip));
        }
    }

    if scored_lines.is_empty() {
        if let Some(t) = title {
            return vec![format!("(matched on title) L1: {t}")];
        } else {
            return vec!["(matched on filename)".to_string()];
        }
    }

    // Sort by line score descending, breaking ties with earlier line number
    scored_lines.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));

    // Take top lines and present them in reading order (ascending line number)
    let mut top_lines: Vec<_> = scored_lines.into_iter().take(MAX_SNIPPETS_PER_NOTE).collect();
    top_lines.sort_by_key(|item| item.1);

    top_lines
        .into_iter()
        .map(|(_, line_num, text)| format!("L{line_num}: {text}"))
        .collect()
}

/// Rank documents and extract contextual snippets for the query.
pub fn rank_and_snippet(query: &str, docs: &[(String, String)]) -> Vec<NoteMatch> {
    let query_clean = query.trim();
    if query_clean.is_empty() || docs.is_empty() {
        return Vec::new();
    }

    let query_lc = query_clean.to_lowercase();
    let query_tokens = tokenize(&query_lc);
    if query_tokens.is_empty() {
        return Vec::new();
    }

    let n = docs.len();

    // Precompute doc lengths and term frequencies for BM25
    let mut doc_tokens: Vec<Vec<String>> = Vec::with_capacity(n);
    let mut doc_tfs: Vec<HashMap<String, usize>> = Vec::with_capacity(n);
    let mut total_tokens = 0usize;

    for (_, content) in docs {
        let toks = tokenize(content);
        let mut tf = HashMap::new();
        for t in &toks {
            *tf.entry(t.clone()).or_insert(0) += 1;
        }
        total_tokens += toks.len();
        doc_tokens.push(toks);
        doc_tfs.push(tf);
    }

    let avgdl = if n > 0 {
        (total_tokens as f64) / (n as f64)
    } else {
        1.0
    }
    .max(1.0);

    // Compute document frequency (df) for each query token
    let mut df: HashMap<String, usize> = HashMap::new();
    for t in &query_tokens {
        let mut count = 0usize;
        for (i, (path, _)) in docs.iter().enumerate() {
            let in_content = doc_tfs[i].contains_key(t);
            let in_path = path.to_lowercase().contains(t.as_str());
            if in_content || in_path {
                count += 1;
            }
        }
        df.insert(t.clone(), count);
    }

    let mut matches = Vec::new();

    for (i, (path, content)) in docs.iter().enumerate() {
        let title = extract_title(path, content);
        let stem = extract_stem(path);
        let path_lc = path.to_lowercase();
        let stem_lc = stem.to_lowercase();
        let title_lc = title.as_deref().unwrap_or("").to_lowercase();
        let stem_tokens = tokenize(&stem_lc);
        let title_tokens = title.as_ref().map(|t| tokenize(t)).unwrap_or_default();

        let mut score = 0.0;
        let mut matched_query_terms = 0usize;

        // 1. Filename & Title Boosting
        if query_lc == stem_lc || (!title_lc.is_empty() && query_lc == title_lc) {
            score += 35.0;
        } else if stem_lc.contains(&query_lc) || (!title_lc.is_empty() && title_lc.contains(&query_lc)) {
            score += 18.0;
        } else if path_lc.contains(&query_lc) {
            score += 10.0;
        }

        for t in &query_tokens {
            let in_stem = stem_tokens.contains(t);
            let in_title = title_tokens.contains(t);
            let in_path = path_lc.contains(t.as_str());
            if in_stem {
                score += 8.0;
            } else if in_title {
                score += 6.0;
            } else if in_path {
                score += 3.0;
            }
        }

        // 2. Exact Phrase Match in Content
        let content_lc = content.to_lowercase();
        if query_tokens.len() > 1 && content_lc.contains(&query_lc) {
            let exact_count = content_lc.matches(&query_lc).count();
            score += 15.0 + (exact_count.min(5) as f64) * 2.0;
        }

        // 3. BM25 Content Scoring
        let dl = doc_tokens[i].len() as f64;
        let mut bm25 = 0.0;

        for t in &query_tokens {
            let tf = doc_tfs[i].get(t).copied().unwrap_or(0);
            let term_in_meta = stem_tokens.contains(t) || title_tokens.contains(t) || path_lc.contains(t.as_str());

            if tf > 0 || term_in_meta {
                matched_query_terms += 1;
            }

            if tf > 0 {
                let dft = df.get(t).copied().unwrap_or(1);
                // BM25+ Robertson IDF formulation with +1 guard
                let idf = (1.0 + (n as f64 - dft as f64 + 0.5) / (dft as f64 + 0.5)).ln().max(0.1);
                let tf_f = tf as f64;
                let denom = tf_f + K1 * (1.0 - B + B * (dl / avgdl));
                bm25 += idf * (tf_f * (K1 + 1.0)) / denom;
            }
        }

        // Query coverage multiplier (penalize matching only 1 of multiple terms)
        if query_tokens.len() > 1 {
            let coverage = (matched_query_terms as f64) / (query_tokens.len() as f64);
            if coverage >= 1.0 {
                bm25 = bm25 * 1.4 + 5.0;
            } else {
                bm25 *= coverage;
            }
        }

        score += bm25;

        // Only return files that actually hit at least one query term
        if score > 0.0 && matched_query_terms > 0 {
            let snippets = extract_snippets(content, title.as_deref(), &query_tokens, &query_lc);
            matches.push(NoteMatch {
                path: path.clone(),
                score,
                snippets,
            });
        }
    }

    // Rank highest score first; break ties by path alphabetical
    matches.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal).then(a.path.cmp(&b.path)));
    matches
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_phrase_beats_scattered_tokens() {
        let docs = vec![
            (
                "scattered.md".to_string(),
                "System architecture is crucial. Later on we discuss the prompt design.".to_string(),
            ),
            (
                "exact.md".to_string(),
                "The system prompt defines the assistant's behavior and persona.".to_string(),
            ),
        ];

        let ranked = rank_and_snippet("system prompt", &docs);
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].path, "exact.md");
        assert!(ranked[0].score > ranked[1].score);
    }

    #[test]
    fn title_and_filename_boosts_ranking() {
        let docs = vec![
            (
                "notes/general.md".to_string(),
                "We discussed many things, and briefly mentioned deepseek.".to_string(),
            ),
            (
                "notes/deepseek.md".to_string(),
                "Brief notes here.".to_string(),
            ),
        ];

        let ranked = rank_and_snippet("deepseek", &docs);
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].path, "notes/deepseek.md");
    }

    #[test]
    fn markdown_heading_title_extracted() {
        let content = "# Machine Learning Basics\nIntroduction to ML.";
        let title = extract_title("docs/intro.md", content);
        assert_eq!(title.as_deref(), Some("Machine Learning Basics"));
    }

    #[test]
    fn html_title_extracted() {
        let content = "<html><head><title>Dashboard App</title></head><body><h1>Welcome</h1></body></html>";
        let title = extract_title("dashboard.html", content);
        assert_eq!(title.as_deref(), Some("Dashboard App"));
    }

    #[test]
    fn windowed_snippet_centers_on_long_line() {
        let long_line = format!(
            "{} DEEPSEEK_SECRET_KEY {}",
            "A".repeat(200),
            "B".repeat(200)
        );
        let snippet = window_snippet(&long_line, &["deepseek_secret_key".to_string()], "");
        assert!(snippet.contains("DEEPSEEK_SECRET_KEY"));
        assert!(snippet.starts_with('…'));
        assert!(snippet.ends_with('…'));
        assert!(snippet.chars().count() <= MAX_SNIPPET_LINE_CHARS + 4);
    }

    #[test]
    fn snippets_include_accurate_line_numbers() {
        let content = "Line 1\nLine 2\nLine 3 with needle\nLine 4\nLine 5 with another needle";
        let snips = extract_snippets(content, None, &["needle".to_string()], "needle");
        assert_eq!(snips.len(), 2);
        assert!(snips[0].starts_with("L3:"));
        assert!(snips[1].starts_with("L5:"));
    }
}
