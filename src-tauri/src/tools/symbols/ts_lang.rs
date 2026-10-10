// ── surface: backend ──────────────────────────────────
// Tree-Sitter symbol extractor for TypeScript, TSX, and JavaScript (.ts, .tsx, .js, .jsx).

use super::cleaner::{clean_sig, strip_comments};
use super::SymbolDef;
use tree_sitter::{Node, Parser};

/// Extract TypeScript / JavaScript symbols using Tree-Sitter AST.
pub fn extract_ts_symbols(src: &str, ext: &str, include_internals: bool) -> Vec<SymbolDef> {
    let mut parser = Parser::new();
    let lang = match ext {
        "ts" | "mts" | "cts" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        _ => tree_sitter_typescript::LANGUAGE_TSX.into(),
    };
    if parser.set_language(&lang).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(src, None) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    walk_ts_root(tree.root_node(), src, ext, include_internals, &mut out);
    out
}

fn walk_ts_root(root: Node, src: &str, ext: &str, include_internals: bool, out: &mut Vec<SymbolDef>) {
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        match child.kind() {
            "export_statement" => {
                handle_export_statement(child, src, ext, out);
            }
            "function_declaration" if include_internals => {
                let header = if let Some(body) = child.child_by_field_name("body") {
                    &src[child.start_byte()..body.start_byte()]
                } else {
                    &src[child.start_byte()..child.end_byte()]
                };
                let stripped = strip_comments(header, ext);
                out.push(SymbolDef {
                    line_no: child.start_position().row + 1,
                    kind: "fn",
                    signature: clean_sig(&stripped, true),
                });
            }
            "class_declaration" if include_internals => {
                let header = if let Some(body) = child.child_by_field_name("body") {
                    &src[child.start_byte()..body.start_byte()]
                } else {
                    &src[child.start_byte()..child.end_byte()]
                };
                let stripped = strip_comments(header, ext);
                out.push(SymbolDef {
                    line_no: child.start_position().row + 1,
                    kind: "class",
                    signature: clean_sig(&stripped, true),
                });
            }
            "interface_declaration" if include_internals => {
                let header = if let Some(body) = child.child_by_field_name("body") {
                    &src[child.start_byte()..body.start_byte()]
                } else {
                    &src[child.start_byte()..child.end_byte()]
                };
                let stripped = strip_comments(header, ext);
                out.push(SymbolDef {
                    line_no: child.start_position().row + 1,
                    kind: "interface",
                    signature: clean_sig(&stripped, true),
                });
            }
            "type_alias_declaration" if include_internals => {
                let header = &src[child.start_byte()..child.end_byte()];
                let stripped = strip_comments(header, ext);
                out.push(SymbolDef {
                    line_no: child.start_position().row + 1,
                    kind: "type",
                    signature: clean_sig(&stripped, true),
                });
            }
            "enum_declaration" if include_internals => {
                let header = if let Some(body) = child.child_by_field_name("body") {
                    &src[child.start_byte()..body.start_byte()]
                } else {
                    &src[child.start_byte()..child.end_byte()]
                };
                let stripped = strip_comments(header, ext);
                out.push(SymbolDef {
                    line_no: child.start_position().row + 1,
                    kind: "enum",
                    signature: clean_sig(&stripped, true),
                });
            }
            "lexical_declaration" | "variable_declaration" if include_internals => {
                handle_const_decl(child, child.start_byte(), src, ext, out);
            }
            _ => {}
        }
    }
}

fn handle_export_statement(export_node: Node, src: &str, ext: &str, out: &mut Vec<SymbolDef>) {
    let mut cursor = export_node.walk();
    for sub in export_node.children(&mut cursor) {
        match sub.kind() {
            "function_declaration" => {
                let header = if let Some(body) = sub.child_by_field_name("body") {
                    &src[export_node.start_byte()..body.start_byte()]
                } else {
                    &src[export_node.start_byte()..sub.end_byte()]
                };
                let stripped = strip_comments(header, ext);
                out.push(SymbolDef {
                    line_no: export_node.start_position().row + 1,
                    kind: "fn",
                    signature: clean_sig(&stripped, true),
                });
            }
            "class_declaration" => {
                let header = if let Some(body) = sub.child_by_field_name("body") {
                    &src[export_node.start_byte()..body.start_byte()]
                } else {
                    &src[export_node.start_byte()..sub.end_byte()]
                };
                let stripped = strip_comments(header, ext);
                out.push(SymbolDef {
                    line_no: export_node.start_position().row + 1,
                    kind: "class",
                    signature: clean_sig(&stripped, true),
                });
            }
            "interface_declaration" => {
                let header = if let Some(body) = sub.child_by_field_name("body") {
                    &src[export_node.start_byte()..body.start_byte()]
                } else {
                    &src[export_node.start_byte()..sub.end_byte()]
                };
                let stripped = strip_comments(header, ext);
                out.push(SymbolDef {
                    line_no: export_node.start_position().row + 1,
                    kind: "interface",
                    signature: clean_sig(&stripped, true),
                });
            }
            "type_alias_declaration" => {
                let header = &src[export_node.start_byte()..export_node.end_byte()];
                let stripped = strip_comments(header, ext);
                out.push(SymbolDef {
                    line_no: export_node.start_position().row + 1,
                    kind: "type",
                    signature: clean_sig(&stripped, true),
                });
            }
            "enum_declaration" => {
                let header = if let Some(body) = sub.child_by_field_name("body") {
                    &src[export_node.start_byte()..body.start_byte()]
                } else {
                    &src[export_node.start_byte()..sub.end_byte()]
                };
                let stripped = strip_comments(header, ext);
                out.push(SymbolDef {
                    line_no: export_node.start_position().row + 1,
                    kind: "enum",
                    signature: clean_sig(&stripped, true),
                });
            }
            "lexical_declaration" | "variable_declaration" => {
                handle_const_decl(sub, export_node.start_byte(), src, ext, out);
            }
            _ => {}
        }
    }
}

fn handle_const_decl(
    decl_node: Node,
    start_byte: usize,
    src: &str,
    ext: &str,
    out: &mut Vec<SymbolDef>,
) {
    let mut cursor = decl_node.walk();
    for sub in decl_node.children(&mut cursor) {
        if sub.kind() == "variable_declarator" {
            let is_multiline = sub.start_position().row != sub.end_position().row;
            let type_node = sub.child_by_field_name("type");
            let name_node = sub.child_by_field_name("name");

            if !is_multiline {
                let raw = &src[start_byte..decl_node.end_byte()];
                let stripped = strip_comments(raw, ext);
                out.push(SymbolDef {
                    line_no: decl_node.start_position().row + 1,
                    kind: "const",
                    signature: clean_sig(&stripped, true),
                });
            } else if let Some(t) = type_node {
                // Multiline with type annotation -> keep up to type annotation end
                let raw = &src[start_byte..t.end_byte()];
                let stripped = strip_comments(raw, ext);
                out.push(SymbolDef {
                    line_no: decl_node.start_position().row + 1,
                    kind: "const",
                    signature: clean_sig(&stripped, true),
                });
            } else if let Some(n) = name_node {
                // Multiline without type annotation -> name = ...
                let raw = format!("{} = ...", &src[start_byte..n.end_byte()]);
                let stripped = strip_comments(&raw, ext);
                out.push(SymbolDef {
                    line_no: decl_node.start_position().row + 1,
                    kind: "const",
                    signature: clean_sig(&stripped, true),
                });
            } else {
                let raw = &src[start_byte..decl_node.end_byte()];
                let stripped = strip_comments(raw, ext);
                out.push(SymbolDef {
                    line_no: decl_node.start_position().row + 1,
                    kind: "const",
                    signature: clean_sig(&stripped, true),
                });
            }
        }
    }
}
