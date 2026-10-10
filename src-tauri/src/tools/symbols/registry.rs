// ── surface: backend ──────────────────────────────────
// Language registry and dispatcher for Tree-Sitter symbol extraction.

use super::cs_lang::extract_cs_symbols;
use super::fallback::extract_symbols_scoped_fallback;
use super::gd_lang::extract_gd_symbols;
use super::go_lang::extract_go_symbols;
use super::py_lang::extract_py_symbols;
use super::rust_lang::extract_rust_symbols;
use super::ts_lang::extract_ts_symbols;
use super::SymbolDef;

/// Extract symbols using Tree-Sitter AST with graceful fallback for unmapped or broken inputs.
pub fn dispatch_extract_symbols(content: &str, ext: &str, include_internals: bool) -> Vec<SymbolDef> {
    let lower_ext = ext.to_ascii_lowercase();
    let symbols = match lower_ext.as_str() {
        "rs" => extract_rust_symbols(content, include_internals),
        "ts" | "mts" | "cts" | "tsx" | "jsx" | "js" | "mjs" | "cjs" => {
            extract_ts_symbols(content, &lower_ext, include_internals)
        }
        "py" | "pyw" => extract_py_symbols(content, include_internals),
        "go" => extract_go_symbols(content, include_internals),
        "gd" => extract_gd_symbols(content, include_internals),
        "cs" => extract_cs_symbols(content, include_internals),
        _ => Vec::new(),
    };

    if !symbols.is_empty() {
        return symbols;
    }

    // Fallback for unmapped extensions or severed/incomplete code fragments
    extract_symbols_scoped_fallback(content, &lower_ext, include_internals)
}
