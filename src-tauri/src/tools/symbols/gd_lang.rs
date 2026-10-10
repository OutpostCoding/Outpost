// ── surface: backend ──────────────────────────────────
// Tree-Sitter symbol extractor for GDScript (.gd).

use super::cleaner::{clean_sig, strip_comments};
use super::SymbolDef;
use tree_sitter::{Node, Parser};

/// Extract GDScript symbols using Tree-Sitter AST.
pub fn extract_gd_symbols(src: &str, include_internals: bool) -> Vec<SymbolDef> {
    let mut parser = Parser::new();
    if parser.set_language(&tree_sitter_gdscript::LANGUAGE.into()).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(src, None) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    walk_gd_nodes(tree.root_node(), src, include_internals, &mut out);
    out
}

fn walk_gd_nodes(node: Node, src: &str, include_internals: bool, out: &mut Vec<SymbolDef>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "class_name_statement" => {
                let header = &src[child.start_byte()..child.end_byte()];
                let stripped = strip_comments(header, "gd");
                out.push(SymbolDef {
                    line_no: child.start_position().row + 1,
                    kind: "class",
                    signature: clean_sig(&stripped, true),
                });
            }
            "signal_statement" => {
                let header = &src[child.start_byte()..child.end_byte()];
                let stripped = strip_comments(header, "gd");
                out.push(SymbolDef {
                    line_no: child.start_position().row + 1,
                    kind: "signal",
                    signature: clean_sig(&stripped, true),
                });
            }
            "function_definition" => {
                let header = if let Some(body) = child.child_by_field_name("body") {
                    &src[child.start_byte()..body.start_byte()]
                } else {
                    &src[child.start_byte()..child.end_byte()]
                };
                let stripped = strip_comments(header, "gd");
                out.push(SymbolDef {
                    line_no: child.start_position().row + 1,
                    kind: "func",
                    signature: clean_sig(&stripped, true),
                });
            }
            "class_definition" => {
                let header = if let Some(body) = child.child_by_field_name("body") {
                    &src[child.start_byte()..body.start_byte()]
                } else {
                    &src[child.start_byte()..child.end_byte()]
                };
                let stripped = strip_comments(header, "gd");
                out.push(SymbolDef {
                    line_no: child.start_position().row + 1,
                    kind: "class",
                    signature: clean_sig(&stripped, true),
                });

                if include_internals {
                    if let Some(body) = child.child_by_field_name("body") {
                        walk_gd_nodes(body, src, include_internals, out);
                    }
                }
            }
            _ => {}
        }
    }
}
