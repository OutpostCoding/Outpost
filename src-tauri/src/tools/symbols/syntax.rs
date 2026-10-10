// ── surface: backend ──────────────────────────────────
// Tree-Sitter AST syntax error checker for Outpost.
// Supports Rust, TypeScript/TSX, JavaScript/JSX, Python, Go, GDScript, and C# in-memory (<1ms).
// Exports: check_syntax, SyntaxErrorInfo · Related: tools/mod.rs, tools/symbols.rs

use tree_sitter::{Node, Parser};

/// Structured diagnostic returned when source code contains syntax errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxErrorInfo {
    pub lang: &'static str,
    pub line: usize,
    pub column: usize,
    pub message: String,
}

impl SyntaxErrorInfo {
    /// Format into a concise diagnostic string: "line {line}:{col}: {message}".
    pub fn format_diagnostic(&self) -> String {
        format!("line {}:{}: {}", self.line, self.column, self.message)
    }
}

/// Check syntax of `src` for the language associated with `ext`.
/// Returns `None` if the syntax is valid or if the extension is not supported by Tree-Sitter.
/// Returns `Some(SyntaxErrorInfo)` if a syntax corruption is detected.
pub fn check_syntax(src: &str, ext: &str) -> Option<SyntaxErrorInfo> {
    let lower = ext.trim_start_matches('.').to_ascii_lowercase();
    let (lang_label, ts_lang) = match lower.as_str() {
        "rs" => ("Rust", tree_sitter_rust::LANGUAGE.into()),
        "ts" | "mts" | "cts" => ("TypeScript", tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
        "tsx" => ("TypeScript (TSX)", tree_sitter_typescript::LANGUAGE_TSX.into()),
        "js" | "mjs" | "cjs" => ("JavaScript", tree_sitter_typescript::LANGUAGE_TSX.into()),
        "jsx" => ("JavaScript (JSX)", tree_sitter_typescript::LANGUAGE_TSX.into()),
        "py" | "pyw" => ("Python", tree_sitter_python::LANGUAGE.into()),
        "go" => ("Go", tree_sitter_go::LANGUAGE.into()),
        "gd" => ("GDScript", tree_sitter_gdscript::LANGUAGE.into()),
        "cs" => ("C#", tree_sitter_c_sharp::LANGUAGE.into()),
        _ => return None,
    };

    let mut parser = Parser::new();
    if parser.set_language(&ts_lang).is_err() {
        return None;
    }

    let tree = parser.parse(src, None)?;
    let root = tree.root_node();

    if !root.has_error() {
        return None;
    }

    let (line, column, message) = find_first_syntax_error(root, src)?;
    Some(SyntaxErrorInfo {
        lang: lang_label,
        line,
        column,
        message,
    })
}

/// Recursively find the earliest syntax error or missing token in the AST.
fn find_first_syntax_error(node: Node, src: &str) -> Option<(usize, usize, String)> {
    if !node.has_error() {
        return None;
    }

    // Missing required token inserted by error recovery (e.g. missing '}', ')', ';')
    if node.is_missing() {
        let pt = node.start_position();
        let kind = node.kind();
        let msg = if !kind.is_empty() {
            format!("missing expected '{}'", kind)
        } else {
            "missing expected token".to_string()
        };
        return Some((pt.row + 1, pt.column + 1, msg));
    }

    // Concrete ERROR node
    if node.is_error() {
        // First check if any direct child is a missing token
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.is_missing() {
                let cpt = child.start_position();
                let kind = child.kind();
                return Some((cpt.row + 1, cpt.column + 1, format!("missing expected '{}'", kind)));
            }
        }

        let pt = node.start_position();
        let s = node.start_byte();
        let e = node.end_byte().min(src.len());
        let snip = if s < e {
            let slice = &src[s..e];
            let first = slice.lines().next().unwrap_or(slice).trim();
            if first.is_empty() {
                "token".to_string()
            } else if first.len() > 32 {
                format!("'{}...'", &first[..32])
            } else {
                format!("'{}'", first)
            }
        } else {
            "token".to_string()
        };

        return Some((pt.row + 1, pt.column + 1, format!("unexpected {}", snip)));
    }

    // Recurse into children in document order
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.has_error() {
            if let Some(err) = find_first_syntax_error(child, src) {
                return Some(err);
            }
        }
    }

    let pt = node.start_position();
    Some((pt.row + 1, pt.column + 1, "syntax error".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_rust_passes_cleanly() {
        let code = "fn main() {\n    println!(\"hello world\");\n}\n";
        assert_eq!(check_syntax(code, "rs"), None);
    }

    #[test]
    fn broken_rust_unclosed_brace_detected() {
        let code = "fn main() {\n    let x = 1;\n";
        let err = check_syntax(code, "rs").expect("should catch syntax error");
        assert_eq!(err.lang, "Rust");
        assert!(err.message.contains("missing") || err.message.contains("syntax error") || err.message.contains("unexpected"));
    }

    #[test]
    fn broken_rust_unexpected_token_detected() {
        let code = "fn main() {\n    let x = ;\n}\n";
        let err = check_syntax(code, "rs").expect("should catch syntax error");
        assert_eq!(err.lang, "Rust");
        assert_eq!(err.line, 2);
    }

    #[test]
    fn valid_typescript_passes_cleanly() {
        let code = "interface User {\n    id: number;\n    name: string;\n}\nconst u: User = { id: 1, name: 'alice' };\n";
        assert_eq!(check_syntax(code, "ts"), None);
    }

    #[test]
    fn broken_typescript_detected() {
        let code = "const x: = 123;\n";
        let err = check_syntax(code, "ts").expect("should catch syntax error");
        assert_eq!(err.lang, "TypeScript");
        assert_eq!(err.line, 1);
    }

    #[test]
    fn valid_python_passes_cleanly() {
        let code = "def greet(name: str) -> str:\n    return f\"Hello, {name}\"\n";
        assert_eq!(check_syntax(code, "py"), None);
    }

    #[test]
    fn broken_python_indentation_or_colon_detected() {
        let code = "def greet(name: str)\n    return f\"Hello, {name}\"\n";
        let err = check_syntax(code, "py").expect("should catch missing colon");
        assert_eq!(err.lang, "Python");
        assert_eq!(err.line, 1);
    }

    #[test]
    fn valid_go_passes_cleanly() {
        let code = "package main\n\nfunc main() {\n\tprintln(\"hello\")\n}\n";
        assert_eq!(check_syntax(code, "go"), None);
    }

    #[test]
    fn broken_go_detected() {
        let code = "package main\n\nfunc main() {\n\tx := \n}\n";
        let err = check_syntax(code, "go").expect("should catch syntax error");
        assert_eq!(err.lang, "Go");
        assert_eq!(err.line, 4);
    }

    #[test]
    fn valid_gdscript_passes_cleanly() {
        let code = "extends Node2D\n\nfunc _ready():\n\tprint(\"ready\")\n";
        assert_eq!(check_syntax(code, "gd"), None);
    }

    #[test]
    fn broken_gdscript_detected() {
        let code = "func _ready(:\n\tpass\n";
        let err = check_syntax(code, "gd").expect("should catch syntax error");
        assert_eq!(err.lang, "GDScript");
        assert_eq!(err.line, 1);
    }

    #[test]
    fn valid_csharp_passes_cleanly() {
        let code = "namespace Foo {\n    public class Bar {\n        public void Test() {}\n    }\n}\n";
        assert_eq!(check_syntax(code, "cs"), None);
    }

    #[test]
    fn broken_csharp_detected() {
        let code = "namespace Foo {\n    public class Bar {\n        public void Test( {}\n    }\n}\n";
        let err = check_syntax(code, "cs").expect("should catch syntax error");
        assert_eq!(err.lang, "C#");
        assert_eq!(err.line, 3);
    }

    #[test]
    fn test_codebase_key_files_parse_cleanly() {
        let app_tsx = std::fs::read_to_string("../src/App.tsx").expect("App.tsx exists");
        assert_eq!(check_syntax(&app_tsx, "tsx"), None, "App.tsx must parse cleanly with zero syntax errors");

        let mod_rs = std::fs::read_to_string("src/tools/mod.rs").expect("mod.rs exists");
        assert_eq!(check_syntax(&mod_rs, "rs"), None, "mod.rs must parse cleanly with zero syntax errors");

        let commands_rs = std::fs::read_to_string("src/commands.rs").expect("commands.rs exists");
        assert_eq!(check_syntax(&commands_rs, "rs"), None, "commands.rs must parse cleanly with zero syntax errors");
    }
}
