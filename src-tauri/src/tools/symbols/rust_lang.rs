// ── surface: backend ──────────────────────────────────
// Tree-Sitter symbol extractor for Rust (.rs).

use super::cleaner::{clean_sig, strip_comments};
use super::SymbolDef;
use tree_sitter::{Node, Parser};

/// Extract Rust symbols using Tree-Sitter AST.
pub fn extract_rust_symbols(src: &str, include_internals: bool) -> Vec<SymbolDef> {
    let mut parser = Parser::new();
    if parser.set_language(&tree_sitter_rust::LANGUAGE.into()).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(src, None) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    walk_rust_node(tree.root_node(), src, include_internals, &mut out);
    out
}

fn walk_rust_node(node: Node, src: &str, include_internals: bool, out: &mut Vec<SymbolDef>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "function_item" => {
                if has_test_attr(child, src) {
                    continue;
                }
                let is_pub = has_pub_visibility(child);
                if !is_pub && !include_internals {
                    continue;
                }
                let header = if let Some(body) = child.child_by_field_name("body") {
                    &src[child.start_byte()..body.start_byte()]
                } else {
                    &src[child.start_byte()..child.end_byte()]
                };
                let stripped = strip_comments(header, "rs");
                out.push(SymbolDef {
                    line_no: child.start_position().row + 1,
                    kind: "fn",
                    signature: clean_sig(&stripped, true),
                });
            }
            "struct_item" => {
                let is_pub = has_pub_visibility(child);
                if !is_pub && !include_internals {
                    continue;
                }
                let header = if let Some(body) = child.child_by_field_name("body") {
                    &src[child.start_byte()..body.start_byte()]
                } else {
                    &src[child.start_byte()..child.end_byte()]
                };
                let stripped = strip_comments(header, "rs");
                out.push(SymbolDef {
                    line_no: child.start_position().row + 1,
                    kind: "struct",
                    signature: clean_sig(&stripped, true),
                });
            }
            "enum_item" => {
                let is_pub = has_pub_visibility(child);
                if !is_pub && !include_internals {
                    continue;
                }
                let header = if let Some(body) = child.child_by_field_name("body") {
                    &src[child.start_byte()..body.start_byte()]
                } else {
                    &src[child.start_byte()..child.end_byte()]
                };
                let stripped = strip_comments(header, "rs");
                out.push(SymbolDef {
                    line_no: child.start_position().row + 1,
                    kind: "enum",
                    signature: clean_sig(&stripped, true),
                });
            }
            "trait_item" => {
                let is_pub = has_pub_visibility(child);
                if !is_pub && !include_internals {
                    continue;
                }
                let header = if let Some(body) = child.child_by_field_name("body") {
                    &src[child.start_byte()..body.start_byte()]
                } else {
                    &src[child.start_byte()..child.end_byte()]
                };
                let stripped = strip_comments(header, "rs");
                out.push(SymbolDef {
                    line_no: child.start_position().row + 1,
                    kind: "trait",
                    signature: clean_sig(&stripped, true),
                });
            }
            "type_item" => {
                let is_pub = has_pub_visibility(child);
                if !is_pub && !include_internals {
                    continue;
                }
                let header = &src[child.start_byte()..child.end_byte()];
                let stripped = strip_comments(header, "rs");
                out.push(SymbolDef {
                    line_no: child.start_position().row + 1,
                    kind: "type",
                    signature: clean_sig(&stripped, true),
                });
            }
            "impl_item" => {
                let has_trait = child.child_by_field_name("trait").is_some();
                if include_internals || has_trait {
                    let header = if let Some(body) = child.child_by_field_name("body") {
                        &src[child.start_byte()..body.start_byte()]
                    } else {
                        &src[child.start_byte()..child.end_byte()]
                    };
                    let stripped = strip_comments(header, "rs");
                    out.push(SymbolDef {
                        line_no: child.start_position().row + 1,
                        kind: "impl",
                        signature: clean_sig(&stripped, true),
                    });
                }
                // When including internals, recurse into impl body for methods
                if include_internals {
                    if let Some(body) = child.child_by_field_name("body") {
                        walk_rust_node(body, src, include_internals, out);
                    }
                }
            }
            "mod_item" => {
                if has_cfg_test_attr(child, src) {
                    continue; // Skip test module and everything inside
                }
                if let Some(body) = child.child_by_field_name("body") {
                    walk_rust_node(body, src, include_internals, out);
                }
            }
            _ => {}
        }
    }
}

fn has_pub_visibility(node: Node) -> bool {
    if let Some(vis) = node.child_by_field_name("visibility") {
        vis.kind() == "visibility_modifier"
    } else {
        false
    }
}

fn has_test_attr(node: Node, src: &str) -> bool {
    let mut prev = node.prev_sibling();
    while let Some(p) = prev {
        if p.kind() == "attribute_item" {
            let text = &src[p.start_byte()..p.end_byte()];
            if text.contains("test") {
                return true;
            }
            prev = p.prev_sibling();
        } else {
            break;
        }
    }
    false
}

fn has_cfg_test_attr(node: Node, src: &str) -> bool {
    let mut prev = node.prev_sibling();
    while let Some(p) = prev {
        if p.kind() == "attribute_item" {
            let text = &src[p.start_byte()..p.end_byte()];
            if text.contains("cfg(test)") || text.contains("cfg(all(test") {
                return true;
            }
            prev = p.prev_sibling();
        } else {
            break;
        }
    }
    false
}
