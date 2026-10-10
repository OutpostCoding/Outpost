// ── surface: backend ──────────────────────────────────
// Fallback regex/lexer-based symbol extractor for unmapped extensions and severed fragments.

use super::cleaner::clean_sig;
use super::SymbolDef;
use regex::Regex;
use std::sync::OnceLock;

static TS_REGEX: OnceLock<Regex> = OnceLock::new();
static TS_INTERNAL_REGEX: OnceLock<Regex> = OnceLock::new();
static RUST_REGEX: OnceLock<Regex> = OnceLock::new();
static RUST_INTERNAL_REGEX: OnceLock<Regex> = OnceLock::new();
static PY_REGEX: OnceLock<Regex> = OnceLock::new();
static GO_REGEX: OnceLock<Regex> = OnceLock::new();
static GO_INTERNAL_REGEX: OnceLock<Regex> = OnceLock::new();
static GD_REGEX: OnceLock<Regex> = OnceLock::new();

fn ts_regex() -> &'static Regex {
    TS_REGEX.get_or_init(|| {
        Regex::new(r"^(?:export\s+(?:default\s+)?(?:async\s+)?function\s+([a-zA-Z0-9_$]+[^{;]*)|export\s+(?:const|let|var)\s+([a-zA-Z0-9_$]+[^=;{]*)|export\s+(?:interface|type|class|enum)\s+([a-zA-Z0-9_$]+[^{;=]*))").unwrap()
    })
}

fn ts_internal_regex() -> &'static Regex {
    TS_INTERNAL_REGEX.get_or_init(|| {
        Regex::new(r"^(?:(?:export\s+(?:default\s+)?)?(?:async\s+)?function\s+([a-zA-Z0-9_$]+[^{;]*)|(?:export\s+)?(?:const|let|var)\s+([a-zA-Z0-9_$]+[^=;{]*)|(?:export\s+)?(?:interface|type|class|enum)\s+([a-zA-Z0-9_$]+[^{;=]*))").unwrap()
    })
}

fn rust_regex() -> &'static Regex {
    RUST_REGEX.get_or_init(|| {
        Regex::new(r"^(?:pub\s+(?:async\s+)?(?:const\s+)?(?:unsafe\s+)?fn\s+([a-zA-Z0-9_]+[^{;]*)|pub\s+(?:struct|enum|trait|type)\s+([a-zA-Z0-9_]+[^{;]*)|impl(?:\s*<[^>]+>)?\s+([a-zA-Z0-9_]+(?:\s*<[^>]+>)?\s+for\s+[a-zA-Z0-9_]+[^{;]*))").unwrap()
    })
}

fn rust_internal_regex() -> &'static Regex {
    RUST_INTERNAL_REGEX.get_or_init(|| {
        Regex::new(r"^(?:(?:pub(?:\([^)]+\))?\s+)?(?:async\s+)?(?:const\s+)?(?:unsafe\s+)?fn\s+([a-zA-Z0-9_]+[^{;]*)|(?:pub(?:\([^)]+\))?\s+)?(?:struct|enum|trait|type)\s+([a-zA-Z0-9_]+[^{;]*)|impl(?:\s*<[^>]+>)?\s+([a-zA-Z0-9_]+(?:\s*<[^>]+>)?(?:\s+for\s+[a-zA-Z0-9_]+)?[^{;]*))").unwrap()
    })
}

fn py_regex() -> &'static Regex {
    PY_REGEX.get_or_init(|| {
        Regex::new(r"^(?:async\s+)?def\s+([a-zA-Z0-9_]+(?:\([^)]*\))?[^:]*)|class\s+([a-zA-Z0-9_]+(?:\([^)]*\))?[^:]*)").unwrap()
    })
}

fn go_regex() -> &'static Regex {
    GO_REGEX.get_or_init(|| {
        Regex::new(r"^func\s+(?:\([^)]+\)\s+)?([A-Z][a-zA-Z0-9_]*[^{]*)|^type\s+([A-Z][a-zA-Z0-9_]*\s+(?:struct|interface|[a-zA-Z0-9_]+)[^{]*)").unwrap()
    })
}

fn go_internal_regex() -> &'static Regex {
    GO_INTERNAL_REGEX.get_or_init(|| {
        Regex::new(r"^func\s+(?:\([^)]+\)\s+)?([a-zA-Z0-9_]+[^{]*)|^type\s+([a-zA-Z0-9_]+\s+(?:struct|interface|[a-zA-Z0-9_]+)[^{]*)").unwrap()
    })
}

fn gd_regex() -> &'static Regex {
    GD_REGEX.get_or_init(|| {
        Regex::new(r"^(?:@\w+\s+)?func\s+([a-zA-Z0-9_]+[^{:]*)|^class_name\s+([a-zA-Z0-9_]+)|^signal\s+([a-zA-Z0-9_]+[^{:]*)").unwrap()
    })
}

const MAX_HEADER_LINES: usize = 40;

#[derive(Clone, Copy)]
enum DeclMode {
    Header,
    Const,
}

#[derive(Default, Clone)]
struct Scan {
    paren: i32,
    bracket: i32,
    brace: i32,
    quote: Option<char>,
    block_comment: bool,
}

fn top_level(st: &Scan) -> bool {
    st.paren == 0 && st.bracket == 0 && st.brace == 0 && st.quote.is_none() && !st.block_comment
}

enum Step {
    Continue,
    Done,
    DoneTrimmed { at: usize, untyped_value: bool },
}

fn ends_with_continuation(s: &str) -> bool {
    let t = s.trim_end();
    t.ends_with(',')
        || t.ends_with('|')
        || t.ends_with('&')
        || t.ends_with('+')
        || t.ends_with('-')
        || t.ends_with('*')
        || t.ends_with('/')
        || t.ends_with('=')
        || t.ends_with('(')
        || t.ends_with('[')
        || t.ends_with('{')
}

fn scan_line(line: &str, st: &mut Scan, mode: DeclMode, ext: &str) -> (String, Step) {
    let mut out = String::with_capacity(line.len());
    let mut terminator = false;
    let mut body_brace = false;
    let mut assign_at: Option<usize> = None;
    let mut typed = false;
    let mut prev: Option<char> = None;
    let mut chars = line.chars().peekable();

    while let Some(c) = chars.next() {
        if st.block_comment {
            if c == '*' && chars.peek() == Some(&'/') {
                chars.next();
                st.block_comment = false;
            }
            continue;
        }
        if let Some(q) = st.quote {
            out.push(c);
            if c == '\\' {
                if let Some(escaped) = chars.next() {
                    out.push(escaped);
                }
            } else if c == q {
                st.quote = None;
            }
            prev = Some(c);
            continue;
        }
        match c {
            '"' | '\'' | '`' => {
                st.quote = Some(c);
                out.push(c);
            }
            '/' => match chars.peek().copied() {
                Some('/') => break,
                Some('*') => {
                    chars.next();
                    st.block_comment = true;
                }
                _ => out.push(c),
            },
            '(' => {
                st.paren += 1;
                out.push(c);
            }
            ')' => {
                st.paren -= 1;
                out.push(c);
            }
            '[' => {
                st.bracket += 1;
                out.push(c);
            }
            ']' => {
                st.bracket -= 1;
                out.push(c);
            }
            '{' => {
                if top_level(st) {
                    body_brace = true;
                }
                st.brace += 1;
                out.push(c);
            }
            '}' => {
                st.brace -= 1;
                out.push(c);
            }
            ';' => {
                if top_level(st) {
                    terminator = true;
                }
                out.push(c);
            }
            ':' => {
                if matches!(ext, "py" | "gd") {
                    if top_level(st) {
                        terminator = true;
                    }
                } else if top_level(st) && assign_at.is_none() {
                    typed = true;
                }
                out.push(c);
            }
            '=' => {
                let compound = matches!(
                    prev,
                    Some('=' | '!' | '<' | '>' | '+' | '-' | '*' | '/' | '&' | '|' | '^')
                );
                if !compound && chars.peek() != Some(&'>') && top_level(st) && assign_at.is_none() {
                    assign_at = Some(out.len());
                }
                out.push(c);
            }
            _ => out.push(c),
        }
        prev = Some(c);
    }

    let step = if terminator {
        Step::Done
    } else {
        match mode {
            DeclMode::Header => {
                if body_brace {
                    Step::Done
                } else {
                    Step::Continue
                }
            }
            DeclMode::Const => {
                if !top_level(st) {
                    match assign_at {
                        Some(at) => Step::DoneTrimmed {
                            at,
                            untyped_value: !typed,
                        },
                        None => Step::Continue,
                    }
                } else if ends_with_continuation(&out) {
                    Step::Continue
                } else {
                    Step::Done
                }
            }
        }
    };
    (out, step)
}

fn is_decl_start(trimmed: &str, ext: &str) -> bool {
    match ext {
        "rs" => {
            trimmed.starts_with("pub ")
                || trimmed.starts_with("pub(")
                || trimmed.starts_with("impl ")
                || trimmed.starts_with("fn ")
                || trimmed.starts_with("async fn ")
                || trimmed.starts_with("const fn ")
                || trimmed.starts_with("unsafe fn ")
                || trimmed.starts_with("struct ")
                || trimmed.starts_with("enum ")
                || trimmed.starts_with("trait ")
                || trimmed.starts_with("type ")
        }
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => {
            trimmed.starts_with("export ")
                || trimmed.starts_with("function ")
                || trimmed.starts_with("async function ")
                || trimmed.starts_with("class ")
                || trimmed.starts_with("interface ")
                || trimmed.starts_with("type ")
                || trimmed.starts_with("const ")
                || trimmed.starts_with("let ")
                || trimmed.starts_with("var ")
        }
        "py" => {
            trimmed.starts_with("def ")
                || trimmed.starts_with("async def ")
                || trimmed.starts_with("class ")
        }
        "go" => trimmed.starts_with("func ") || trimmed.starts_with("type "),
        "gd" => {
            trimmed.starts_with("func ")
                || trimmed.starts_with("class_name ")
                || trimmed.starts_with("signal ")
        }
        _ => false,
    }
}

fn accumulate_signature(lines: &[&str], start_idx: usize, ext: &str) -> (String, bool) {
    let first = lines[start_idx].trim();
    let mode = if (matches!(ext, "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs")
        && (first.starts_with("export const ")
            || first.starts_with("export let ")
            || first.starts_with("export var ")
            || first.starts_with("const ")
            || first.starts_with("let ")
            || first.starts_with("var ")))
        || first.starts_with("export const ")
        || first.starts_with("export let ")
        || first.starts_with("export var ")
    {
        DeclMode::Const
    } else {
        DeclMode::Header
    };

    let mut st = Scan::default();
    let mut out = String::new();
    let mut complete = false;
    let mut used = 0usize;
    let mut j = start_idx;

    while j < lines.len() {
        let raw = lines[j].trim();
        if j > start_idx && (raw.is_empty() || is_decl_start(raw, ext)) {
            break;
        }

        let (line_code, step) = scan_line(raw, &mut st, mode, ext);
        let line_code = line_code.trim();

        if line_code.is_empty() {
            match step {
                Step::Continue => {
                    j += 1;
                    continue;
                }
                _ => {
                    complete = true;
                    break;
                }
            }
        }

        used += 1;
        if !out.is_empty() {
            out.push(' ');
        }
        let base = out.len();
        out.push_str(line_code);

        match step {
            Step::Continue => {}
            Step::Done => {
                complete = true;
                break;
            }
            Step::DoneTrimmed { at, untyped_value } => {
                out.truncate(base + at);
                if untyped_value {
                    out.push_str(" = ...");
                }
                complete = true;
                break;
            }
        }

        if used >= MAX_HEADER_LINES {
            break;
        }
        j += 1;
    }

    if !complete && top_level(&st) {
        complete = true;
    }
    (out, complete)
}

pub fn extract_symbols_scoped_fallback(
    content: &str,
    ext: &str,
    include_internals: bool,
) -> Vec<SymbolDef> {
    let mut out = Vec::new();
    let lower_ext = ext.to_ascii_lowercase();
    let lines: Vec<&str> = content.lines().collect();

    match lower_ext.as_str() {
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => {
            let re = if include_internals {
                ts_internal_regex()
            } else {
                ts_regex()
            };
            for (idx, &line) in lines.iter().enumerate() {
                let trimmed = line.trim_start();
                let is_top_level = !line.starts_with(' ') && !line.starts_with('\t');
                let eligible = if include_internals {
                    trimmed.starts_with("export") || is_top_level
                } else {
                    trimmed.starts_with("export")
                };

                if eligible && re.is_match(trimmed) {
                    let kind = if trimmed.contains("function") {
                        "fn"
                    } else if trimmed.contains("interface") {
                        "interface"
                    } else if trimmed.contains("type ") {
                        "type"
                    } else if trimmed.contains("class") {
                        "class"
                    } else if trimmed.contains("enum") {
                        "enum"
                    } else {
                        "const"
                    };
                    let (raw, complete) = accumulate_signature(&lines, idx, &lower_ext);
                    out.push(SymbolDef {
                        line_no: idx + 1,
                        kind,
                        signature: clean_sig(&raw, complete),
                    });
                }
            }
        }
        "rs" => {
            let re = if include_internals {
                rust_internal_regex()
            } else {
                rust_regex()
            };
            for (idx, &line) in lines.iter().enumerate() {
                let trimmed = line.trim_start();
                let is_top_level = !line.starts_with(' ') && !line.starts_with('\t');
                let eligible = if include_internals {
                    trimmed.starts_with("pub") || is_top_level
                } else {
                    trimmed.starts_with("pub")
                };

                if eligible && re.is_match(trimmed) {
                    let kind = if trimmed.contains("fn ") {
                        "fn"
                    } else if trimmed.contains("struct ") {
                        "struct"
                    } else if trimmed.contains("enum ") {
                        "enum"
                    } else if trimmed.contains("trait ") {
                        "trait"
                    } else if trimmed.contains("type ") {
                        "type"
                    } else {
                        "impl"
                    };
                    let (raw, complete) = accumulate_signature(&lines, idx, &lower_ext);
                    out.push(SymbolDef {
                        line_no: idx + 1,
                        kind,
                        signature: clean_sig(&raw, complete),
                    });
                }
            }
        }
        "py" => {
            let re = py_regex();
            for (idx, &line) in lines.iter().enumerate() {
                let is_top_level = !line.starts_with(' ') && !line.starts_with('\t') && !line.is_empty();
                if (is_top_level || include_internals) && re.is_match(line.trim_start()) {
                    let trimmed = line.trim_start();
                    let kind = if trimmed.starts_with("class") { "class" } else { "def" };
                    let (raw, complete) = accumulate_signature(&lines, idx, &lower_ext);
                    out.push(SymbolDef {
                        line_no: idx + 1,
                        kind,
                        signature: clean_sig(&raw, complete),
                    });
                }
            }
        }
        "go" => {
            let re = if include_internals {
                go_internal_regex()
            } else {
                go_regex()
            };
            for (idx, &line) in lines.iter().enumerate() {
                if !line.starts_with(' ') && !line.starts_with('\t') && !line.is_empty() {
                    let trimmed = line.trim_start();
                    if include_internals && (trimmed.starts_with("func Test") || trimmed.starts_with("func Benchmark")) {
                        continue;
                    }
                    if re.is_match(trimmed) {
                        let kind = if trimmed.starts_with("func") { "func" } else { "type" };
                        let (raw, complete) = accumulate_signature(&lines, idx, &lower_ext);
                        out.push(SymbolDef {
                            line_no: idx + 1,
                            kind,
                            signature: clean_sig(&raw, complete),
                        });
                    }
                }
            }
        }
        "gd" => {
            let re = gd_regex();
            for (idx, &line) in lines.iter().enumerate() {
                let trimmed = line.trim_start();
                if re.is_match(trimmed) {
                    let kind = if trimmed.contains("class_name") {
                        "class"
                    } else if trimmed.contains("signal") {
                        "signal"
                    } else {
                        "func"
                    };
                    let (raw, complete) = accumulate_signature(&lines, idx, &lower_ext);
                    out.push(SymbolDef {
                        line_no: idx + 1,
                        kind,
                        signature: clean_sig(&raw, complete),
                    });
                }
            }
        }
        _ => {}
    }

    out
}
