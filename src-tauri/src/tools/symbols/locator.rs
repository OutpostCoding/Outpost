// ── surface: backend ──────────────────────────────────
// Tree-Sitter AST symbol range locator for Outpost.
// Locates the byte span and line range of named functions, methods, classes, and types.
// Powers Superpower C (Symbol-Scoped Disambiguation) and Superpower D (Full Symbol Replacement).
// Exports: find_symbol, SymbolLocation, SymbolLookupError · Related: tools/symbols.rs, tools/mod.rs

use tree_sitter::{Node, Parser};

/// Concrete location and span of a symbol in source code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolLocation {
    pub name: String,
    pub qualified_name: String,
    pub kind: &'static str,
    pub start_byte: usize,
    pub end_byte: usize,
    pub outer_start_byte: usize,
    pub outer_end_byte: usize,
    pub start_line: usize, // 1-indexed
    pub end_line: usize,   // 1-indexed
    pub outer_start_line: usize,
    pub outer_end_line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolMatchInfo {
    pub qualified_name: String,
    pub line_no: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymbolLookupError {
    UnsupportedLanguage(String),
    ParseFailed,
    NotFound {
        symbol: String,
        available_symbols: Vec<String>,
    },
    Ambiguous {
        symbol: String,
        matches: Vec<SymbolMatchInfo>,
    },
}

impl std::fmt::Display for SymbolLookupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SymbolLookupError::UnsupportedLanguage(ext) => {
                write!(f, "symbol editing is not supported for '.{ext}' files")
            }
            SymbolLookupError::ParseFailed => write!(f, "could not parse file with Tree-Sitter"),
            SymbolLookupError::NotFound {
                symbol,
                available_symbols,
            } => {
                if available_symbols.is_empty() {
                    write!(f, "symbol '{symbol}' was not found (no symbols detected in file)")
                } else {
                    let preview: Vec<_> = available_symbols.iter().take(8).cloned().collect();
                    let suffix = if available_symbols.len() > 8 {
                        format!(" ... (+{} more)", available_symbols.len() - 8)
                    } else {
                        String::new()
                    };
                    write!(
                        f,
                        "symbol '{symbol}' was not found. Available symbols: {}{}",
                        preview.join(", "),
                        suffix
                    )
                }
            }
            SymbolLookupError::Ambiguous { symbol, matches } => {
                let list = matches
                    .iter()
                    .map(|m| format!("'{}' (line {})", m.qualified_name, m.line_no))
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(
                    f,
                    "symbol '{symbol}' is ambiguous — appears {times} times: {list}. Please specify the qualified name (e.g. 'Class.method').",
                    times = matches.len(),
                    list = list
                )
            }
        }
    }
}

/// Find a symbol by name or qualified name (e.g. "my_fn", "MyClass.my_fn", "MyClass::my_fn").
pub fn find_symbol(
    src: &str,
    ext: &str,
    target_symbol: &str,
) -> Result<SymbolLocation, SymbolLookupError> {
    let lower_ext = ext.trim_start_matches('.').to_ascii_lowercase();
    let ts_lang = match lower_ext.as_str() {
        "rs" => tree_sitter_rust::LANGUAGE.into(),
        "ts" | "mts" | "cts" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "tsx" | "js" | "mjs" | "cjs" | "jsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "py" | "pyw" => tree_sitter_python::LANGUAGE.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        "gd" => tree_sitter_gdscript::LANGUAGE.into(),
        "cs" => tree_sitter_c_sharp::LANGUAGE.into(),
        _ => return Err(SymbolLookupError::UnsupportedLanguage(lower_ext)),
    };

    let mut parser = Parser::new();
    if parser.set_language(&ts_lang).is_err() {
        return Err(SymbolLookupError::ParseFailed);
    }
    let tree = parser.parse(src, None).ok_or(SymbolLookupError::ParseFailed)?;

    let mut all_symbols = Vec::new();
    let mut scope_stack = Vec::new();
    collect_symbols_recursive(tree.root_node(), src, &lower_ext, &mut scope_stack, &mut all_symbols);

    let norm_target = target_symbol.replace("::", ".");
    let target_clean = norm_target.trim();

    // 1. Try exact qualified name match first
    let qualified_matches: Vec<_> = all_symbols
        .iter()
        .filter(|s| s.qualified_name.replace("::", ".") == target_clean)
        .cloned()
        .collect();

    if qualified_matches.len() == 1 {
        return Ok(qualified_matches[0].clone());
    }

    // 2. Try simple name match (e.g. "my_method")
    let name_matches: Vec<_> = all_symbols
        .iter()
        .filter(|s| s.name == target_clean)
        .cloned()
        .collect();

    if name_matches.len() == 1 {
        return Ok(name_matches[0].clone());
    }

    if name_matches.len() > 1 {
        return Err(SymbolLookupError::Ambiguous {
            symbol: target_symbol.to_string(),
            matches: name_matches
                .iter()
                .map(|m| SymbolMatchInfo {
                    qualified_name: m.qualified_name.clone(),
                    line_no: m.start_line,
                })
                .collect(),
        });
    }

    // Not found
    let available = all_symbols
        .iter()
        .map(|s| {
            if s.qualified_name != s.name {
                s.qualified_name.clone()
            } else {
                s.name.clone()
            }
        })
        .collect();

    Err(SymbolLookupError::NotFound {
        symbol: target_symbol.to_string(),
        available_symbols: available,
    })
}

fn collect_symbols_recursive(
    node: Node,
    src: &str,
    ext: &str,
    scope_stack: &mut Vec<String>,
    out: &mut Vec<SymbolLocation>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let kind_str = child.kind();

        // Check if this child defines a symbol
        if let Some((name, custom_qual, kind, is_scope_container)) = inspect_symbol_node(child, src, ext) {
            let qualified_name = if let Some(q) = custom_qual {
                q
            } else if scope_stack.is_empty() {
                name.clone()
            } else {
                format!("{}.{}", scope_stack.join("."), name)
            };

            // Calculate outer bounds (including decorators or export wrapper if present)
            let (outer_start_byte, outer_end_byte, outer_start_line, outer_end_line) = if let Some(parent) = child.parent() {
                if matches!(
                    parent.kind(),
                    "decorated_definition" | "export_statement"
                ) {
                    (
                        parent.start_byte(),
                        parent.end_byte(),
                        parent.start_position().row + 1,
                        parent.end_position().row + 1,
                    )
                } else {
                    (
                        child.start_byte(),
                        child.end_byte(),
                        child.start_position().row + 1,
                        child.end_position().row + 1,
                    )
                }
            } else {
                (
                    child.start_byte(),
                    child.end_byte(),
                    child.start_position().row + 1,
                    child.end_position().row + 1,
                )
            };

            let start_line = child.start_position().row + 1;
            let end_line = child.end_position().row + 1;

            out.push(SymbolLocation {
                name: name.clone(),
                qualified_name,
                kind,
                start_byte: child.start_byte(),
                end_byte: child.end_byte(),
                outer_start_byte,
                outer_end_byte,
                start_line,
                end_line,
                outer_start_line,
                outer_end_line,
            });

            if is_scope_container {
                scope_stack.push(name);
                collect_symbols_recursive(child, src, ext, scope_stack, out);
                scope_stack.pop();
                continue;
            }
        } else if kind_str == "impl_item" && ext == "rs" {
            // Rust impl block: extract type name for scope stack
            let impl_name = child
                .child_by_field_name("type")
                .and_then(|t| src.get(t.start_byte()..t.end_byte()))
                .map(|s| s.trim().to_string());
            if let Some(t_name) = impl_name {
                scope_stack.push(t_name);
                collect_symbols_recursive(child, src, ext, scope_stack, out);
                scope_stack.pop();
                continue;
            }
        }

        // Recurse into children
        collect_symbols_recursive(child, src, ext, scope_stack, out);
    }
}

/// Inspect a node to determine if it defines a named symbol.
/// Returns (name, custom_qualified_name, kind_label, is_scope_container).
fn inspect_symbol_node(node: Node, src: &str, ext: &str) -> Option<(String, Option<String>, &'static str, bool)> {
    let kind = node.kind();
    match ext {
        "rs" => match kind {
            "function_item" => {
                let name = extract_field_text(node, "name", src)?;
                Some((name, None, "fn", false))
            }
            "struct_item" => {
                let name = extract_field_text(node, "name", src)?;
                Some((name, None, "struct", true))
            }
            "enum_item" => {
                let name = extract_field_text(node, "name", src)?;
                Some((name, None, "enum", false))
            }
            "trait_item" => {
                let name = extract_field_text(node, "name", src)?;
                Some((name, None, "trait", true))
            }
            "type_item" => {
                let name = extract_field_text(node, "name", src)?;
                Some((name, None, "type", false))
            }
            "mod_item" => {
                let name = extract_field_text(node, "name", src)?;
                Some((name, None, "mod", true))
            }
            _ => None,
        },
        "py" | "pyw" => match kind {
            "function_definition" => {
                let name = extract_field_text(node, "name", src)?;
                Some((name, None, "fn", false))
            }
            "class_definition" => {
                let name = extract_field_text(node, "name", src)?;
                Some((name, None, "class", true))
            }
            _ => None,
        },
        "ts" | "mts" | "cts" | "tsx" | "jsx" | "js" | "mjs" | "cjs" => match kind {
            "function_declaration" => {
                let name = extract_field_text(node, "name", src)?;
                Some((name, None, "fn", false))
            }
            "class_declaration" => {
                let name = extract_field_text(node, "name", src)?;
                Some((name, None, "class", true))
            }
            "interface_declaration" => {
                let name = extract_field_text(node, "name", src)?;
                Some((name, None, "interface", false))
            }
            "type_alias_declaration" => {
                let name = extract_field_text(node, "name", src)?;
                Some((name, None, "type", false))
            }
            "enum_declaration" => {
                let name = extract_field_text(node, "name", src)?;
                Some((name, None, "enum", false))
            }
            "method_definition" => {
                let name = extract_field_text(node, "name", src)?;
                Some((name, None, "method", false))
            }
            _ => None,
        },
        "go" => match kind {
            "function_declaration" => {
                let name = extract_field_text(node, "name", src)?;
                Some((name, None, "fn", false))
            }
            "method_declaration" => {
                let name = extract_field_text(node, "name", src)?;
                // Method receiver type if present (e.g. "(s *Server) Run()" -> Server.Run)
                let rcvr_type = node
                    .child_by_field_name("receiver")
                    .and_then(|r| src.get(r.start_byte()..r.end_byte()))
                    .map(|r| {
                        r.replace('*', "")
                            .replace(['(', ')'], "")
                            .trim()
                            .to_string()
                    });
                if let Some(r_name) = rcvr_type {
                    let type_prefix = r_name.split_whitespace().last().unwrap_or(&r_name);
                    let full_name = format!("{}.{}", type_prefix, name);
                    Some((name, Some(full_name), "fn", false))
                } else {
                    Some((name, None, "fn", false))
                }
            }
            "type_spec" => {
                let name = extract_field_text(node, "name", src)?;
                Some((name, None, "type", false))
            }
            _ => None,
        },
        "gd" => match kind {
            "function_definition" => {
                let name = extract_field_text(node, "name", src)?;
                Some((name, None, "fn", false))
            }
            "class_name_statement" => {
                let name = extract_field_text(node, "name", src)?;
                Some((name, None, "class", false))
            }
            "signal_statement" => {
                let name = extract_field_text(node, "name", src)?;
                Some((name, None, "signal", false))
            }
            _ => None,
        },
        "cs" => match kind {
            "method_declaration" => {
                let name = extract_field_text(node, "name", src)?;
                Some((name, None, "fn", false))
            }
            "class_declaration" => {
                let name = extract_field_text(node, "name", src)?;
                Some((name, None, "class", true))
            }
            "interface_declaration" => {
                let name = extract_field_text(node, "name", src)?;
                Some((name, None, "interface", false))
            }
            "struct_declaration" => {
                let name = extract_field_text(node, "name", src)?;
                Some((name, None, "struct", true))
            }
            "enum_declaration" => {
                let name = extract_field_text(node, "name", src)?;
                Some((name, None, "enum", false))
            }
            "property_declaration" => {
                let name = extract_field_text(node, "name", src)?;
                Some((name, None, "prop", false))
            }
            _ => None,
        },
        _ => None,
    }
}

fn extract_field_text(node: Node, field: &str, src: &str) -> Option<String> {
    let child = node.child_by_field_name(field)?;
    let text = src.get(child.start_byte()..child.end_byte())?.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_python_function_and_class_method() {
        let code = r#"
class UserService:
    def get_user(self, user_id: str):
        return None

def standalone():
    pass
"#;
        let loc = find_symbol(code, "py", "UserService.get_user").expect("find qualified");
        assert_eq!(loc.name, "get_user");
        assert_eq!(loc.kind, "fn");
        assert!(code[loc.start_byte..loc.end_byte].contains("def get_user"));

        let standalone = find_symbol(code, "py", "standalone").expect("find simple");
        assert_eq!(standalone.name, "standalone");
        assert_eq!(standalone.start_line, 6);
    }

    #[test]
    fn disambiguates_repeated_python_methods_using_class() {
        let code = r#"
class Alpha:
    def process(self):
        pass

class Beta:
    def process(self):
        pass
"#;
        // Simple name is ambiguous
        let err = find_symbol(code, "py", "process").unwrap_err();
        match err {
            SymbolLookupError::Ambiguous { symbol, matches } => {
                assert_eq!(symbol, "process");
                assert_eq!(matches.len(), 2);
            }
            _ => panic!("expected Ambiguous error"),
        }

        // Qualified name resolves uniquely
        let loc = find_symbol(code, "py", "Alpha.process").expect("resolve qualified");
        assert_eq!(loc.qualified_name, "Alpha.process");
        assert_eq!(loc.start_line, 3);
    }

    #[test]
    fn finds_rust_function_and_impl_method() {
        let code = r#"
struct Engine;

impl Engine {
    pub fn start(&self) {
        println!("started");
    }
}

pub fn standalone_fn() {}
"#;
        let loc = find_symbol(code, "rs", "Engine.start").expect("find impl method");
        assert_eq!(loc.name, "start");
        assert_eq!(loc.qualified_name, "Engine.start");

        let fn_loc = find_symbol(code, "rs", "standalone_fn").expect("find standalone");
        assert_eq!(fn_loc.name, "standalone_fn");
    }

    #[test]
    fn reports_available_symbols_when_not_found() {
        let code = "def foo(): pass\ndef bar(): pass\n";
        let err = find_symbol(code, "py", "baz").unwrap_err();
        match err {
            SymbolLookupError::NotFound { symbol, available_symbols } => {
                assert_eq!(symbol, "baz");
                assert!(available_symbols.contains(&"foo".to_string()));
                assert!(available_symbols.contains(&"bar".to_string()));
            }
            _ => panic!("expected NotFound"),
        }
    }

    #[test]
    fn finds_typescript_class_and_method() {
        let code = r#"
export class AppRouter {
    navigate(to: string): void {
        console.log(to);
    }
}

export function helper(): boolean {
    return true;
}
"#;
        let loc = find_symbol(code, "ts", "AppRouter.navigate").expect("find ts method");
        assert_eq!(loc.name, "navigate");
        assert_eq!(loc.qualified_name, "AppRouter.navigate");

        let fn_loc = find_symbol(code, "ts", "helper").expect("find ts export fn");
        assert_eq!(fn_loc.name, "helper");
    }

    #[test]
    fn finds_go_receiver_method() {
        let code = r#"
package server

type Router struct{}

func (r *Router) HandleRoute(path string) {
    println(path)
}

func Init() {}
"#;
        let loc = find_symbol(code, "go", "Router.HandleRoute").expect("find go receiver method");
        assert_eq!(loc.name, "HandleRoute");
        assert_eq!(loc.qualified_name, "Router.HandleRoute");

        let init_loc = find_symbol(code, "go", "Init").expect("find go plain fn");
        assert_eq!(init_loc.name, "Init");
    }

    #[test]
    fn outer_bounds_include_python_decorators() {
        let code = r#"
@dataclass
class Snake:
    x: int
"#;
        let loc = find_symbol(code, "py", "Snake").expect("find Snake");
        assert_eq!(loc.start_line, 3);
        assert_eq!(loc.outer_start_line, 2);
    }
}

