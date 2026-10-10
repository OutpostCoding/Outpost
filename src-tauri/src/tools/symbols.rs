// ── surface: backend ──────────────────────────────────
// Tree-Sitter AST symbol engine for code_symbols tool.
// Supports Rust, TypeScript/TSX/JS, Python, Go, GDScript, and C# with scope filtering.
// Exports: extract_symbols, extract_symbols_scoped, is_supported_code_file, run_code_symbols · Related: tools/mod.rs

use std::path::Path;

mod cleaner;
mod cs_lang;
mod fallback;
mod gd_lang;
mod go_lang;
mod py_lang;
mod registry;
mod rust_lang;
pub mod locator;
pub mod syntax;
mod ts_lang;

#[allow(unused_imports)]
pub use locator::{find_symbol, SymbolLocation, SymbolLookupError};
#[allow(unused_imports)]
pub use syntax::{check_syntax, SyntaxErrorInfo};

/// Single exported or internal symbol signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolDef {
    pub line_no: usize,
    pub kind: &'static str,
    pub signature: String,
}

/// Extract symbols from source file content based on extension (public/exported only).
pub fn extract_symbols(content: &str, ext: &str) -> Vec<SymbolDef> {
    extract_symbols_scoped(content, ext, false)
}

/// Extract symbols from source file content with scope-based internal symbol inclusion.
/// When `include_internals` is true (single-file inspection), internal functions, types,
/// and methods are captured, while unit test functions and modules are skipped.
pub fn extract_symbols_scoped(content: &str, ext: &str, include_internals: bool) -> Vec<SymbolDef> {
    registry::dispatch_extract_symbols(content, ext, include_internals)
}

/// Supported source file extensions.
pub fn is_supported_code_file(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".ts")
        || lower.ends_with(".tsx")
        || lower.ends_with(".js")
        || lower.ends_with(".jsx")
        || lower.ends_with(".rs")
        || lower.ends_with(".py")
        || lower.ends_with(".pyw")
        || lower.ends_with(".go")
        || lower.ends_with(".gd")
        || lower.ends_with(".cs")
}

/// Execute `code_symbols` across `root` with optional path filter.
pub fn run_code_symbols(project_root: Option<&Path>, path_arg: Option<&str>) -> String {
    let Some(root) = project_root else {
        return "error: no project is open — code_symbols needs an active coding session.".into();
    };

    let target = path_arg.map(|s| s.trim()).filter(|s| !s.is_empty());

    // Single file targeted
    if let Some(rel) = target {
        let clean = rel.replace('\\', "/").trim_start_matches('/').to_string();
        if clean.contains("..") || Path::new(&clean).has_root() || Path::new(&clean).components().any(|c| matches!(c, std::path::Component::Prefix(_))) {
            return format!("error: path '{rel}' is not a valid relative path within the project.");
        }
        let full_path = root.join(&clean);
        if let Ok(canon_root) = root.canonicalize() {
            if let Ok(canon_p) = full_path.canonicalize() {
                if !canon_p.starts_with(&canon_root) {
                    return format!("error: path '{rel}' resolves outside the project folder.");
                }
            }
        }
        if full_path.is_file() {
            let ext = full_path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if !is_supported_code_file(rel) {
                return format!("'{rel}' is not a supported source code file (supports .ts, .tsx, .js, .jsx, .rs, .py, .go, .gd, .cs).");
            }
            let Ok(content) = std::fs::read_to_string(&full_path) else {
                return format!("error: could not read file '{rel}'.");
            };
            let syms = extract_symbols_scoped(&content, ext, true);
            if syms.is_empty() {
                return format!("(no symbols found in '{rel}')");
            }
            let mut out = format!("{rel}:\n");
            for s in syms {
                out.push_str(&format!("  {}\n", s.signature));
            }
            return out;
        }
    }

    // Directory or full project walk
    let scan_dir = if let Some(rel) = target {
        let clean = rel.replace('\\', "/").trim_start_matches('/').to_string();
        if clean.contains("..") || Path::new(&clean).has_root() || Path::new(&clean).components().any(|c| matches!(c, std::path::Component::Prefix(_))) {
            return format!("error: path '{rel}' is not a valid relative path within the project.");
        }
        let p = root.join(&clean);
        if let Ok(canon_root) = root.canonicalize() {
            if let Ok(canon_p) = p.canonicalize() {
                if !canon_p.starts_with(&canon_root) {
                    return format!("error: path '{rel}' resolves outside the project folder.");
                }
            }
        }
        if !p.exists() {
            return format!("error: path '{rel}' does not exist in project.");
        }
        if !p.is_dir() {
            return format!("error: '{rel}' is neither a file nor a directory.");
        }
        p
    } else {
        root.to_path_buf()
    };

    let mut candidate_files = Vec::new();
    super::Tools::walk_project(&scan_dir, root, &mut candidate_files, 500);

    let code_files: Vec<String> = candidate_files
        .into_iter()
        .filter(|p| is_supported_code_file(p))
        .collect();

    if code_files.is_empty() {
        if let Some(rel) = target {
            return format!("(no supported source files found in '{rel}')");
        } else {
            return "(no supported source files found in project)".into();
        }
    }

    let mut total_symbols = 0usize;
    let max_symbols = 80usize;
    let mut out = String::new();

    for rel in &code_files {
        if total_symbols >= max_symbols {
            break;
        }
        let full = root.join(rel);
        let ext = full.extension().and_then(|e| e.to_str()).unwrap_or("");
        let Ok(content) = std::fs::read_to_string(&full) else {
            continue;
        };
        let syms = extract_symbols(&content, ext);
        if syms.is_empty() {
            continue;
        }

        out.push_str(&format!("{rel}:\n"));
        for s in syms {
            out.push_str(&format!("  {}\n", s.signature));
            total_symbols += 1;
            if total_symbols >= max_symbols {
                break;
            }
        }
        out.push('\n');
    }

    if total_symbols == 0 {
        return "(no exported symbols found in scanned files)".into();
    }

    if total_symbols >= max_symbols {
        out.push_str("… (symbol map capped; specify a path, e.g. find_symbols path:\"src/hooks\", to inspect deeper)\n");
    }

    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_typescript_exported_symbols() {
        let src = r#"
import { useState } from "react";

export interface FileManagerOptions {
    autoSync: boolean;
}

export type FileId = string | number;

export const DEFAULT_CAP = 5000;

export function useFileManager(options: FileManagerOptions) {
    return { ok: true };
}

export async function syncFiles(): Promise<void> {
    // body
}

export class CacheManager {
    // internal
}

function internalHelper() {
    return 1;
}
"#;
        let syms = extract_symbols(src, "ts");
        let sigs: Vec<&str> = syms.iter().map(|s| s.signature.as_str()).collect();

        assert!(sigs.contains(&"export interface FileManagerOptions"));
        assert!(sigs.contains(&"export type FileId = string | number"));
        assert!(sigs.contains(&"export const DEFAULT_CAP = 5000"));
        assert!(sigs.contains(&"export function useFileManager(options: FileManagerOptions)"));
        assert!(sigs.contains(&"export async function syncFiles(): Promise<void>"));
        assert!(sigs.contains(&"export class CacheManager"));
        assert!(!sigs.iter().any(|s| s.contains("internalHelper")));
    }

    #[test]
    fn extracts_rust_public_symbols() {
        let src = r#"
pub struct AppState {
    pub count: usize,
}

pub enum RunOutcome {
    Success,
    Failed(String),
}

pub trait Provider: Send + Sync {
    fn name(&self) -> &str;
}

pub async fn send_chat(req: ChatRequest) -> Result<(), String> {
    Ok(())
}

fn private_helper() -> i32 {
    42
}
"#;
        let syms = extract_symbols(src, "rs");
        let sigs: Vec<&str> = syms.iter().map(|s| s.signature.as_str()).collect();

        assert!(sigs.contains(&"pub struct AppState"));
        assert!(sigs.contains(&"pub enum RunOutcome"));
        assert!(sigs.contains(&"pub trait Provider: Send + Sync"));
        assert!(sigs.contains(&"pub async fn send_chat(req: ChatRequest) -> Result<(), String>"));
        assert!(!sigs.iter().any(|s| s.contains("private_helper")));
    }

    #[test]
    fn extracts_python_top_level_symbols() {
        let src = r#"
import os

class ModelRunner:
    def __init__(self):
        pass

def load_config(path: str) -> dict:
    return {}

async def fetch_data(url: str):
    pass
"#;
        let syms = extract_symbols(src, "py");
        let sigs: Vec<&str> = syms.iter().map(|s| s.signature.as_str()).collect();

        assert!(sigs.contains(&"class ModelRunner"));
        assert!(sigs.contains(&"def load_config(path: str) -> dict"));
        assert!(sigs.contains(&"async def fetch_data(url: str)"));
        // Method inside class should not be treated as top-level exported function
        assert!(!sigs.contains(&"def __init__(self)"));
    }

    #[test]
    fn run_code_symbols_file_and_dir() {
        let temp_dir = std::env::temp_dir().join(format!("outpost_sym_test_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(temp_dir.join("src")).unwrap();

        let ts_code = "export function hello() {\n}\nfunction internalHelper() {\n}\nexport const FOO = 42;\nconst INTERNAL_NUM = 7;\n";
        std::fs::write(temp_dir.join("src").join("lib.ts"), ts_code).unwrap();

        // Direct single file: includes internals
        let res = run_code_symbols(Some(&temp_dir), Some("src/lib.ts"));
        assert!(res.contains("src/lib.ts:"), "Output was: {res}");
        assert!(res.contains("export function hello()"), "Output was: {res}");
        assert!(res.contains("function internalHelper()"), "Output was: {res}");
        assert!(res.contains("export const FOO = 42"), "Output was: {res}");
        assert!(res.contains("const INTERNAL_NUM = 7"), "Output was: {res}");

        // Directory walk: exports-only
        let res_dir = run_code_symbols(Some(&temp_dir), Some("src"));
        assert!(res_dir.contains("src/lib.ts:"), "Output was: {res_dir}");
        assert!(res_dir.contains("export function hello()"), "Output was: {res_dir}");
        assert!(res_dir.contains("export const FOO = 42"), "Output was: {res_dir}");
        assert!(!res_dir.contains("internalHelper"), "Directory walk should not contain internalHelper: {res_dir}");
        assert!(!res_dir.contains("INTERNAL_NUM"), "Directory walk should not contain INTERNAL_NUM: {res_dir}");

        // Project root scan: exports-only
        let res_all = run_code_symbols(Some(&temp_dir), None);
        assert!(res_all.contains("src/lib.ts:"), "Output was: {res_all}");
        assert!(res_all.contains("export function hello()"), "Output was: {res_all}");
        assert!(!res_all.contains("internalHelper"), "Project scan should not contain internalHelper: {res_all}");

        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn run_code_symbols_rejects_escaping_paths() {
        let temp_dir = std::env::temp_dir().join(format!(
            "outpost_sym_test_esc_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp_dir).unwrap();

        // Traversal
        let res = run_code_symbols(Some(&temp_dir), Some("../outside.rs"));
        assert!(res.contains("error:"), "Expected error for traversal, got: {res}");

        // Absolute path
        #[cfg(windows)]
        {
            let res_abs = run_code_symbols(
                Some(&temp_dir),
                Some(r"C:\Windows\System32\drivers\etc\hosts"),
            );
            assert!(
                res_abs.contains("error:"),
                "Expected error for absolute path, got: {res_abs}"
            );
        }

        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn extracts_rust_multiline_wrapped_signatures() {
        let src = r#"
pub fn start_argv(
    &self,
    argv: &[String],
    display: &str,
    cwd: &Path,
    cwd_label: &str,
) -> Result<u64, String> {
    Ok(1)
}

pub struct ComplexType<
    T: Send + Sync,
    U: Clone,
> {
    pub item: T,
}
"#;
        let syms = extract_symbols(src, "rs");
        assert_eq!(syms.len(), 2);
        assert_eq!(
            syms[0].signature,
            "pub fn start_argv(&self, argv: &[String], display: &str, cwd: &Path, cwd_label: &str) -> Result<u64, String>"
        );
        assert_eq!(syms[0].line_no, 2);
        assert!(syms[1].signature.contains("pub struct ComplexType"));
    }

    #[test]
    fn extracts_typescript_multiline_signatures() {
        let src = r#"
export async function fetchUserData(
    userId: string,
    options?: RequestOptions,
): Promise<UserData> {
    return {};
}

export type ComplexUnion =
    | 'option_a'
    | 'option_b';
"#;
        let syms = extract_symbols(src, "ts");
        assert_eq!(syms.len(), 2);
        assert_eq!(
            syms[0].signature,
            "export async function fetchUserData(userId: string, options?: RequestOptions): Promise<UserData>"
        );
        assert_eq!(syms[0].line_no, 2);
        assert!(syms[1].signature.contains("export type ComplexUnion ="));
    }

    #[test]
    fn extracts_python_multiline_signatures() {
        let src = r#"
def process_data(
    records: list[dict],
    verbose: bool = False,
) -> SummaryResult:
    pass
"#;
        let syms = extract_symbols(src, "py");
        assert_eq!(syms.len(), 1);
        assert_eq!(
            syms[0].signature,
            "def process_data(records: list[dict], verbose: bool = False) -> SummaryResult"
        );
        assert_eq!(syms[0].line_no, 2);
    }

    #[test]
    fn marks_incomplete_multiline_with_ellipsis() {
        let mut src = String::from("export function GiantComponent({\n");
        for i in 0..60 {
            src.push_str(&format!("    prop_{i},\n"));
        }
        src.push_str("}: GiantProps) {\n    return null;\n}\n");

        let syms = extract_symbols(&src, "ts");
        assert_eq!(syms.len(), 1);
        assert!(
            syms[0].signature.ends_with("..."),
            "Truncated signature must end with ellipsis: {}",
            syms[0].signature
        );
        assert!(syms[0].signature.starts_with("export function GiantComponent({"));
    }

    #[test]
    fn marks_early_cutoff_with_ellipsis() {
        let src = "export function IncompleteComponent({\n    first,\n    second,\n\n    otherCode();\n";
        let syms = extract_symbols(src, "ts");
        assert_eq!(syms.len(), 1);
        assert_eq!(
            syms[0].signature,
            "export function IncompleteComponent({ first, second, ..."
        );
    }

    #[test]
    fn extracts_real_composer_component_signature() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("src")
            .join("components")
            .join("composer")
            .join("Composer.tsx");
        if path.exists() {
            let content = std::fs::read_to_string(&path).unwrap();
            let syms = extract_symbols(&content, "tsx");
            assert_eq!(syms.len(), 1);
            assert!(syms[0].signature.starts_with("export function Composer({"));
            assert!(
                syms[0].signature.ends_with("..."),
                "Must end with ...: {}",
                syms[0].signature
            );
        }
    }

    #[test]
    fn drops_line_comments_between_parameters() {
        let src = "export function Widget({\n    // Topbar\n    preview,\n    other,\n}: Props) {\n    return null;\n}\n";
        let syms = extract_symbols(src, "tsx");
        assert_eq!(syms.len(), 1);
        assert!(
            !syms[0].signature.contains("//"),
            "comment bled into the signature: {}",
            syms[0].signature
        );
        assert!(
            syms[0].signature.contains("preview"),
            "parameter after the comment was lost: {}",
            syms[0].signature
        );
    }

    #[test]
    fn drops_block_comments_between_parameters() {
        let src = "export async function send(\n    request: Req,\n    /** Gates the vision tools. */\n    visionOn: boolean,\n): Promise<void> {\n}\n";
        let syms = extract_symbols(src, "ts");
        assert_eq!(syms.len(), 1);
        assert_eq!(
            syms[0].signature,
            "export async function send(request: Req, visionOn: boolean): Promise<void>"
        );
    }

    #[test]
    fn string_literals_are_not_mistaken_for_comments_or_brackets() {
        let src = "export function client(url: string = \"http://localhost:1234/v1\", body = \"{}\") {\n    return null;\n}\n";
        let syms = extract_symbols(src, "ts");
        assert_eq!(syms.len(), 1);
        assert_eq!(
            syms[0].signature,
            "export function client(url: string = \"http://localhost:1234/v1\", body = \"{}\")"
        );
    }

    #[test]
    fn untyped_const_with_multiline_value_is_marked_as_having_one() {
        let src = "export const CODE_CORE = [\n    \"project_list\",\n    \"project_search\",\n];\n";
        let syms = extract_symbols(src, "ts");
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].signature, "export const CODE_CORE = ...");
    }

    #[test]
    fn single_line_const_keeps_its_value() {
        let src = "export const FULL_COPY = 1_000_000_000;\n";
        let syms = extract_symbols(src, "ts");
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].signature, "export const FULL_COPY = 1_000_000_000");
    }

    #[test]
    fn const_with_multiline_initializer_reports_the_declared_type() {
        let src = "export const CONNECTIONS: Connection[] = [\n    { id: \"lmstudio\", baseUrl: \"http://localhost:1234/v1\" },\n    { id: \"openai\", baseUrl: \"https://api.openai.com/v1\" },\n];\n";
        let syms = extract_symbols(src, "ts");
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].signature, "export const CONNECTIONS: Connection[]");
    }

    #[test]
    fn complete_and_incomplete_truncation_are_distinguishable() {
        let params: Vec<String> = (0..40).map(|i| format!("p{i}: string")).collect();
        let wide = format!(
            "export function wide({}) {{\n    return null;\n}}\n",
            params.join(", ")
        );
        let syms = extract_symbols(&wide, "ts");
        assert_eq!(syms.len(), 1);
        assert!(syms[0].signature.ends_with("..."), "{}", syms[0].signature);
        assert!(
            !syms[0].signature.ends_with(", ..."),
            "a complete declaration must not be marked as continuing: {}",
            syms[0].signature
        );

        let cut = "export function cut({\n    a,\n    b,\n\n    otherCode();\n";
        let syms = extract_symbols(cut, "ts");
        assert_eq!(syms.len(), 1);
        assert!(
            syms[0].signature.ends_with(", ..."),
            "an incomplete declaration must be marked as continuing: {}",
            syms[0].signature
        );
    }

    #[test]
    fn real_component_signatures_carry_no_comment_text() {
        let src_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("src");
        if !src_dir.exists() {
            return;
        }

        let mut checked = 0usize;
        let mut stack = vec![src_dir];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                if !matches!(ext, "ts" | "tsx") {
                    continue;
                }
                let Ok(content) = std::fs::read_to_string(&path) else {
                    continue;
                };
                for sym in extract_symbols(&content, ext) {
                    assert!(
                        !sym.signature.contains("// ") && !sym.signature.contains("/*"),
                        "comment text in signature ({}:{}): {}",
                        path.display(),
                        sym.line_no,
                        sym.signature
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 0, "sweep found no symbols to check");
    }

    #[test]
    fn scoped_extraction_typescript_includes_internals() {
        let src = r#"
export function exportedFn() {
    return 1;
}
function internalFn() {
    return 2;
}
const INTERNAL_CONST = 42;
export const EXPORTED_CONST = 100;
interface InternalInterface {
    name: string;
}
export interface ExportedInterface {
    id: number;
}
class InternalClass {
}
export class ExportedClass {
}
"#;
        let public_syms = extract_symbols(src, "ts");
        let public_sigs: Vec<&str> = public_syms.iter().map(|s| s.signature.as_str()).collect();
        assert!(public_sigs.contains(&"export function exportedFn()"));
        assert!(public_sigs.contains(&"export const EXPORTED_CONST = 100"));
        assert!(public_sigs.contains(&"export interface ExportedInterface"));
        assert!(public_sigs.contains(&"export class ExportedClass"));
        assert!(!public_sigs.iter().any(|s| s.contains("internalFn")));
        assert!(!public_sigs.iter().any(|s| s.contains("INTERNAL_CONST")));
        assert!(!public_sigs.iter().any(|s| s.contains("InternalInterface")));
        assert!(!public_sigs.iter().any(|s| s.contains("InternalClass")));

        let scoped_syms = extract_symbols_scoped(src, "ts", true);
        let scoped_sigs: Vec<&str> = scoped_syms.iter().map(|s| s.signature.as_str()).collect();
        assert!(scoped_sigs.contains(&"export function exportedFn()"));
        assert!(scoped_sigs.contains(&"function internalFn()"));
        assert!(scoped_sigs.contains(&"const INTERNAL_CONST = 42"));
        assert!(scoped_sigs.contains(&"interface InternalInterface"));
        assert!(scoped_sigs.contains(&"class InternalClass"));
    }

    #[test]
    fn scoped_extraction_rust_fixture_strings_do_not_leak() {
        let src = r####"
pub fn public_fn() {
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inner() {
        let fixture = r#"
fn fixture_fn() {
}
"#;
        assert!(fixture.contains("fixture_fn"));
    }

    fn test_helper() {
    }
}

fn after_test_module() -> u8 {
    7
}
"####;
        let scoped = extract_symbols_scoped(src, "rs", true);
        let sigs: Vec<String> = scoped.iter().map(|s| s.signature.clone()).collect();
        assert!(sigs.iter().any(|s| s.contains("public_fn")), "{sigs:?}");
        assert!(
            sigs.iter().any(|s| s.contains("after_test_module")),
            "mask closed too early: {sigs:?}"
        );
        assert!(
            !sigs.iter().any(|s| s.contains("fixture_fn")),
            "fixture text leaked: {sigs:?}"
        );
        assert!(!sigs.iter().any(|s| s.contains("test_helper")), "{sigs:?}");
        assert!(!sigs.iter().any(|s| s.contains("inner")), "{sigs:?}");
    }

    #[test]
    fn scoped_extraction_rust_bare_test_body_masked() {
        let src = r####"
#[test]
fn top_level_test() {
    let fixture = r#"
fn leaked() {
}
"#;
}

pub fn real() {}
"####;
        let scoped = extract_symbols_scoped(src, "rs", true);
        let sigs: Vec<String> = scoped.iter().map(|s| s.signature.clone()).collect();
        assert!(sigs.iter().any(|s| s.contains("real")), "{sigs:?}");
        assert!(
            !sigs.iter().any(|s| s.contains("leaked")),
            "fixture text leaked: {sigs:?}"
        );
        assert!(!sigs.iter().any(|s| s.contains("top_level_test")), "{sigs:?}");
    }

    #[test]
    fn scoped_extraction_rust_includes_internals_and_excludes_tests() {
        let src = r#"
pub fn public_fn() {
}

fn internal_worker() -> bool {
    true
}

async fn internal_task() {
}

struct InternalData {
    val: usize,
}

impl InternalData {
    fn helper(&self) {
    }
}

#[test]
fn test_something() {
    assert!(true);
}

#[tokio::test]
async fn test_async() {
    assert!(true);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_in_mod() {
    }

    fn test_helper() {
    }
}

fn after_test_module() {
}
"#;
        let public_syms = extract_symbols(src, "rs");
        let public_sigs: Vec<&str> = public_syms.iter().map(|s| s.signature.as_str()).collect();
        assert_eq!(public_sigs.len(), 1);
        assert!(public_sigs.contains(&"pub fn public_fn()"));

        let scoped_syms = extract_symbols_scoped(src, "rs", true);
        let scoped_sigs: Vec<&str> = scoped_syms.iter().map(|s| s.signature.as_str()).collect();

        // Internals present
        assert!(scoped_sigs.contains(&"pub fn public_fn()"));
        assert!(scoped_sigs.contains(&"fn internal_worker() -> bool"));
        assert!(scoped_sigs.contains(&"async fn internal_task()"));
        assert!(scoped_sigs.contains(&"struct InternalData"));
        assert!(scoped_sigs.contains(&"impl InternalData"));
        assert!(scoped_sigs.contains(&"fn helper(&self)"));
        assert!(scoped_sigs.contains(&"fn after_test_module()"));

        // Unit tests and test module contents strictly excluded
        assert!(!scoped_sigs.iter().any(|s| s.contains("test_something")));
        assert!(!scoped_sigs.iter().any(|s| s.contains("test_async")));
        assert!(!scoped_sigs.iter().any(|s| s.contains("test_in_mod")));
        assert!(!scoped_sigs.iter().any(|s| s.contains("test_helper")));
    }

    #[test]
    fn scoped_extraction_go_includes_internals_and_skips_tests() {
        let src = r#"
package main

func ExportedHandler() {
}

func internalHelper() {
}

type ExportedType struct {
}

type privateType struct {
}

func TestSomething(t *testing.T) {
}

func BenchmarkSomething(b *testing.B) {
}
"#;
        let public_syms = extract_symbols(src, "go");
        let public_sigs: Vec<&str> = public_syms.iter().map(|s| s.signature.as_str()).collect();
        assert!(public_sigs.contains(&"func ExportedHandler()"));
        assert!(public_sigs.contains(&"type ExportedType struct"));
        assert!(!public_sigs.iter().any(|s| s.contains("internalHelper")));
        assert!(!public_sigs.iter().any(|s| s.contains("privateType")));

        let scoped_syms = extract_symbols_scoped(src, "go", true);
        let scoped_sigs: Vec<&str> = scoped_syms.iter().map(|s| s.signature.as_str()).collect();
        assert!(scoped_sigs.contains(&"func ExportedHandler()"));
        assert!(scoped_sigs.contains(&"func internalHelper()"));
        assert!(scoped_sigs.contains(&"type ExportedType struct"));
        assert!(scoped_sigs.contains(&"type privateType struct"));
        // Tests skipped
        assert!(!scoped_sigs.iter().any(|s| s.contains("TestSomething")));
        assert!(!scoped_sigs.iter().any(|s| s.contains("BenchmarkSomething")));
    }

    #[test]
    fn extracts_gdscript_symbols() {
        let src = r#"
class_name PlayerController
extends CharacterBody3D

signal health_changed(new_health: int)

@rpc("any_peer")
func sync_position(pos: Vector3) -> void:
    pass

func _physics_process(delta: float) -> void:
    pass
"#;
        let syms = extract_symbols(src, "gd");
        let sigs: Vec<&str> = syms.iter().map(|s| s.signature.as_str()).collect();
        assert!(sigs.contains(&"class_name PlayerController"));
        assert!(sigs.contains(&"signal health_changed(new_health: int)"));
        assert!(sigs.contains(&"func sync_position(pos: Vector3) -> void"));
        assert!(sigs.contains(&"func _physics_process(delta: float) -> void"));
    }

    #[test]
    fn extracts_csharp_symbols_and_excludes_tests() {
        let src = r#"
namespace Game.Core {
    public class PlayerManager : IManager {
        public int PlayerCount => 4;

        public async Task<bool> SpawnPlayerAsync(string id, Vector3 spawnPoint) {
            return true;
        }

        private void RecalculateInternals() {
        }

        [Test]
        public void TestSpawnPlayer() {
        }

        [Fact]
        public void FactVerifyState() {
        }
    }

    internal class SecretEngine {
        void InternalTick() {}
    }
}
"#;
        // Public scan (e.g. project / directory walk)
        let public_syms = extract_symbols(src, "cs");
        let public_sigs: Vec<&str> = public_syms.iter().map(|s| s.signature.as_str()).collect();
        assert!(public_sigs.contains(&"public class PlayerManager : IManager"));
        assert!(!public_sigs.iter().any(|s| s.contains("SecretEngine")));
        assert!(!public_sigs.iter().any(|s| s.contains("TestSpawnPlayer")));

        // Scoped inspection (single file target)
        let scoped_syms = extract_symbols_scoped(src, "cs", true);
        let scoped_sigs: Vec<&str> = scoped_syms.iter().map(|s| s.signature.as_str()).collect();
        assert!(scoped_sigs.contains(&"public class PlayerManager : IManager"));
        assert!(scoped_sigs.contains(&"public async Task<bool> SpawnPlayerAsync(string id, Vector3 spawnPoint)"));
        assert!(scoped_sigs.contains(&"private void RecalculateInternals()"));
        assert!(scoped_sigs.contains(&"internal class SecretEngine"));
        assert!(scoped_sigs.contains(&"void InternalTick()"));

        // Unit tests excluded even in scoped inspection
        assert!(!scoped_sigs.iter().any(|s| s.contains("TestSpawnPlayer")));
        assert!(!scoped_sigs.iter().any(|s| s.contains("FactVerifyState")));
    }

    #[test]
    fn test_live_code_symbols_on_project_files() {
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        // Single file scan
        let res = run_code_symbols(Some(repo_root), Some("src-tauri/src/tools/symbols/cs_lang.rs"));
        assert!(res.contains("cs_lang.rs:"), "Output was: {res}");
        assert!(res.contains("pub fn extract_cs_symbols(src: &str, include_internals: bool) -> Vec<SymbolDef>"), "Output was: {res}");

        // Directory walk
        let res_dir = run_code_symbols(Some(repo_root), Some("src-tauri/src/tools/symbols"));
        assert!(res_dir.contains("extract_rust_symbols") || res_dir.contains("extract_cs_symbols"), "Output was: {res_dir}");
    }
}
