// ── surface: backend ──────────────────────────────────
// Tree-Sitter symbol extractor for Go (.go).

use super::cleaner::{clean_sig, strip_comments};
use super::SymbolDef;
use tree_sitter::{Node, Parser};

/// Extract Go symbols using Tree-Sitter AST.
pub fn extract_go_symbols(src: &str, include_internals: bool) -> Vec<SymbolDef> {
    let mut parser = Parser::new();
    if parser.set_language(&tree_sitter_go::LANGUAGE.into()).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(src, None) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    walk_go_nodes(tree.root_node(), src, include_internals, &mut out);
    out
}

fn walk_go_nodes(node: Node, src: &str, include_internals: bool, out: &mut Vec<SymbolDef>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "function_declaration" | "method_declaration" => {
                let name = child
                    .child_by_field_name("name")
                    .map(|n| &src[n.start_byte()..n.end_byte()])
                    .unwrap_or("");

                let is_exported = name.chars().next().map_or(false, |c| c.is_ascii_uppercase());
                if !is_exported && !include_internals {
                    continue;
                }
                if name.starts_with("Test") || name.starts_with("Benchmark") {
                    continue; // Skip unit test and benchmark functions
                }

                let header = if let Some(body) = child.child_by_field_name("body") {
                    &src[child.start_byte()..body.start_byte()]
                } else {
                    &src[child.start_byte()..child.end_byte()]
                };
                let stripped = strip_comments(header, "go");
                out.push(SymbolDef {
                    line_no: child.start_position().row + 1,
                    kind: "func",
                    signature: clean_sig(&stripped, true),
                });
            }
            "type_declaration" => {
                let mut dcursor = child.walk();
                for spec in child.children(&mut dcursor) {
                    if spec.kind() == "type_spec" {
                        let name = spec
                            .child_by_field_name("name")
                            .map(|n| &src[n.start_byte()..n.end_byte()])
                            .unwrap_or("");
                        let is_exported = name.chars().next().map_or(false, |c| c.is_ascii_uppercase());
                        if !is_exported && !include_internals {
                            continue;
                        }

                        // Extract header up to the struct/interface body if present
                        let header = if let Some(type_node) = spec.child_by_field_name("type") {
                            let mut body_start = None;
                            let mut tcursor = type_node.walk();
                            for tc in type_node.children(&mut tcursor) {
                                if tc.kind() == "field_declaration_list" || tc.kind() == "method_spec_list" || tc.kind() == "{" {
                                    body_start = Some(tc.start_byte());
                                    break;
                                }
                            }
                            if let Some(bs) = body_start {
                                &src[child.start_byte()..bs]
                            } else {
                                &src[child.start_byte()..spec.end_byte()]
                            }
                        } else {
                            &src[child.start_byte()..spec.end_byte()]
                        };

                        let stripped = strip_comments(header, "go");
                        out.push(SymbolDef {
                            line_no: child.start_position().row + 1,
                            kind: "type",
                            signature: clean_sig(&stripped, true),
                        });
                    }
                }
            }
            _ => {}
        }
    }
}
