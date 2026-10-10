// ── surface: backend ──────────────────────────────────
// Tree-Sitter symbol extractor for C# (.cs).

use super::cleaner::{clean_sig, strip_comments};
use super::SymbolDef;
use tree_sitter::{Node, Parser};

/// Extract C# symbols using Tree-Sitter AST.
pub fn extract_cs_symbols(src: &str, include_internals: bool) -> Vec<SymbolDef> {
    let mut parser = Parser::new();
    if parser.set_language(&tree_sitter_c_sharp::LANGUAGE.into()).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(src, None) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    walk_cs_nodes(tree.root_node(), src, include_internals, &mut out);
    out
}

fn walk_cs_nodes(node: Node, src: &str, include_internals: bool, out: &mut Vec<SymbolDef>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "namespace_declaration" | "file_scoped_namespace_declaration" => {
                if let Some(body) = child.child_by_field_name("body") {
                    walk_cs_nodes(body, src, include_internals, out);
                } else {
                    walk_cs_nodes(child, src, include_internals, out);
                }
            }
            "class_declaration" => {
                handle_type_decl(child, "class", src, include_internals, out);
            }
            "interface_declaration" => {
                handle_type_decl(child, "interface", src, include_internals, out);
            }
            "struct_declaration" => {
                handle_type_decl(child, "struct", src, include_internals, out);
            }
            "enum_declaration" => {
                handle_type_decl(child, "enum", src, include_internals, out);
            }
            "record_declaration" => {
                handle_type_decl(child, "record", src, include_internals, out);
            }
            "method_declaration" => {
                handle_method_decl(child, src, include_internals, out);
            }
            _ => {}
        }
    }
}

fn handle_type_decl(
    node: Node,
    kind: &'static str,
    src: &str,
    include_internals: bool,
    out: &mut Vec<SymbolDef>,
) {
    let (is_pub, start_node) = inspect_modifiers(node, src);
    if !is_pub && !include_internals {
        return;
    }

    let header = if let Some(body) = node.child_by_field_name("body") {
        &src[start_node.start_byte()..body.start_byte()]
    } else {
        &src[start_node.start_byte()..node.end_byte()]
    };

    let stripped = strip_comments(header, "cs");
    out.push(SymbolDef {
        line_no: start_node.start_position().row + 1,
        kind,
        signature: clean_sig(&stripped, true),
    });

    if include_internals {
        if let Some(body) = node.child_by_field_name("body") {
            walk_cs_nodes(body, src, include_internals, out);
        }
    }
}

fn handle_method_decl(
    node: Node,
    src: &str,
    include_internals: bool,
    out: &mut Vec<SymbolDef>,
) {
    if is_cs_test_method(node, src) {
        return; // Skip unit test methods
    }

    let (is_pub, start_node) = inspect_modifiers(node, src);
    if !is_pub && !include_internals {
        return;
    }

    let header = if let Some(body) = node.child_by_field_name("body") {
        &src[start_node.start_byte()..body.start_byte()]
    } else {
        &src[start_node.start_byte()..node.end_byte()]
    };

    let stripped = strip_comments(header, "cs");
    out.push(SymbolDef {
        line_no: start_node.start_position().row + 1,
        kind: "func",
        signature: clean_sig(&stripped, true),
    });
}

fn inspect_modifiers<'a>(node: Node<'a>, src: &str) -> (bool, Node<'a>) {
    let mut is_pub = false;
    let mut first_code_node = None;

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "attribute_list" {
            continue; // Skip attribute lists to find actual declaration start
        }
        if first_code_node.is_none() {
            first_code_node = Some(child);
        }
        if child.kind() == "modifier" {
            let mod_text = &src[child.start_byte()..child.end_byte()];
            if mod_text == "public" {
                is_pub = true;
            }
        }
    }

    (is_pub, first_code_node.unwrap_or(node))
}

fn is_cs_test_method(node: Node, src: &str) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "attribute_list" {
            let text = &src[child.start_byte()..child.end_byte()];
            if text.contains("Test") || text.contains("Fact") || text.contains("Theory") {
                return true;
            }
        }
    }
    false
}
