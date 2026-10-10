// ── surface: backend ──────────────────────────────────
// Tree-Sitter symbol extractor for Python (.py, .pyw).

use super::cleaner::{clean_sig, strip_comments};
use super::SymbolDef;
use tree_sitter::{Node, Parser};

/// Extract Python symbols using Tree-Sitter AST.
pub fn extract_py_symbols(src: &str, include_internals: bool) -> Vec<SymbolDef> {
    let mut parser = Parser::new();
    if parser.set_language(&tree_sitter_python::LANGUAGE.into()).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(src, None) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    walk_py_nodes(tree.root_node(), src, include_internals, true, &mut out);
    out
}

fn walk_py_nodes(
    node: Node,
    src: &str,
    include_internals: bool,
    is_top_level: bool,
    out: &mut Vec<SymbolDef>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "function_definition" => {
                handle_function(child, src, include_internals, is_top_level, out);
            }
            "class_definition" => {
                handle_class(child, src, include_internals, is_top_level, out);
            }
            "decorated_definition" => {
                let mut dcursor = child.walk();
                for inner in child.children(&mut dcursor) {
                    if inner.kind() == "function_definition" {
                        handle_function(inner, src, include_internals, is_top_level, out);
                    } else if inner.kind() == "class_definition" {
                        handle_class(inner, src, include_internals, is_top_level, out);
                    }
                }
            }
            _ => {}
        }
    }
}

fn handle_function(
    node: Node,
    src: &str,
    include_internals: bool,
    is_top_level: bool,
    out: &mut Vec<SymbolDef>,
) {
    if !is_top_level && !include_internals {
        return;
    }
    let name = node
        .child_by_field_name("name")
        .map(|n| &src[n.start_byte()..n.end_byte()])
        .unwrap_or("");

    if name.starts_with("test_") {
        return; // Exclude test functions
    }

    let header = if let Some(body) = node.child_by_field_name("body") {
        &src[node.start_byte()..body.start_byte()]
    } else {
        &src[node.start_byte()..node.end_byte()]
    };

    let stripped = strip_comments(header, "py");
    out.push(SymbolDef {
        line_no: node.start_position().row + 1,
        kind: "def",
        signature: clean_sig(&stripped, true),
    });
}

fn handle_class(
    node: Node,
    src: &str,
    include_internals: bool,
    _is_top_level: bool,
    out: &mut Vec<SymbolDef>,
) {
    let header = if let Some(body) = node.child_by_field_name("body") {
        &src[node.start_byte()..body.start_byte()]
    } else {
        &src[node.start_byte()..node.end_byte()]
    };

    let stripped = strip_comments(header, "py");
    out.push(SymbolDef {
        line_no: node.start_position().row + 1,
        kind: "class",
        signature: clean_sig(&stripped, true),
    });

    if include_internals {
        if let Some(body) = node.child_by_field_name("body") {
            walk_py_nodes(body, src, include_internals, false, out);
        }
    }
}
