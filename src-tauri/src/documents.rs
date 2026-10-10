// ── surface: backend ──────────────────────────────────
// Uploaded .docx / .pdf → markdown the model can read.
// Exports: extract_document · Related: commands.rs, hooks/useFileManager.ts

//! Document reading — turn an uploaded `.docx` / `.pdf` into markdown/plain text
//! the model can read. This is folded into the existing "Upload file" path (no
//! new toggle): the frontend sends the raw bytes here, we return markdown, and it
//! is saved as a note exactly like a `.md`/`.txt` upload. Document GENERATION
//! (markdown → .docx) is a separate feature and lives elsewhere.
//!
//! - **docx**: unzip → parse `word/document.xml`, mapping each paragraph to a
//!   markdown line (Heading styles → `#`, list items → `-`). Documents that style
//!   their headings *inline* (no Heading `pStyle`) come through as plain
//!   paragraphs — the text is intact, just not marked up; fine for reading.
//! - **pdf**: text extraction via `pdf-extract` (good for real text PDFs; a
//!   scanned/image-only PDF has no text layer, so OCR is a future addition).

use std::io::Read;

use anyhow::{anyhow, Context, Result};
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

/// Extract an uploaded document to markdown/plain text, dispatched by extension.
/// Errors are returned as strings so the webview can surface them to the user.
///
/// `async` deliberately. A non-async `#[tauri::command]` compiles to the BLOCKING
/// path and runs inline on the WebView2 IPC thread, so a slow parse freezes the
/// UI — and `pdf_extract` panics rather than erroring on encodings it doesn't
/// handle (including the spec-standard `/StandardEncoding`), which would abort
/// the whole process and take any in-flight turn with it. `spawn_blocking` moves
/// it off the IPC thread; `catch_unwind` turns the panic into a message.
#[tauri::command]
pub async fn extract_document(filename: String, bytes: Vec<u8>) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let lower = filename.to_lowercase();
        let parse = std::panic::AssertUnwindSafe(|| {
            if lower.ends_with(".docx") {
                docx_to_markdown(&bytes)
            } else if lower.ends_with(".pdf") {
                pdf_to_text(&bytes)
            } else {
                Err(anyhow!("unsupported document type (expected .docx or .pdf)"))
            }
        });
        match std::panic::catch_unwind(parse) {
            Ok(result) => result.map_err(|e| e.to_string()),
            Err(_) => Err(
                "this file couldn't be read — the parser failed on it. If it's a PDF, it may \
                 use an unusual text encoding; re-saving or exporting it again often helps."
                    .to_string(),
            ),
        }
    })
    .await
    .map_err(|e| format!("document reader stopped unexpectedly: {e}"))?
}

/// Unzip a `.docx` and walk `word/document.xml`, emitting one markdown line per
/// Word paragraph.
fn docx_to_markdown(bytes: &[u8]) -> Result<String> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .context("not a valid .docx (it isn't a readable zip archive)")?;
    let mut xml = String::new();
    zip.by_name("word/document.xml")
        .context("no word/document.xml inside the .docx")?
        .read_to_string(&mut xml)?;

    // quick-xml doesn't trim text by default, which is what we want: real text
    // lives inside <w:t> (with xml:space="preserve"), and we only collect text
    // while inside a <w:t>, so inter-element whitespace is ignored anyway.
    let mut reader = Reader::from_str(&xml);

    let mut out = String::new();
    let mut para = String::new();
    let mut style: Option<String> = None;
    let mut is_list = false;
    let mut in_text = false;
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => match e.name().as_ref() {
                b"w:p" => {
                    para.clear();
                    style = None;
                    is_list = false;
                }
                b"w:t" => in_text = true,
                b"w:pStyle" => {
                    if let Some(v) = attr_val(&e, b"w:val") {
                        style = Some(v);
                    }
                }
                b"w:numPr" => is_list = true,
                _ => {}
            },
            Ok(Event::Empty(e)) => match e.name().as_ref() {
                // pStyle is usually self-closing; numPr occasionally so.
                b"w:pStyle" => {
                    if let Some(v) = attr_val(&e, b"w:val") {
                        style = Some(v);
                    }
                }
                b"w:numPr" => is_list = true,
                _ => {}
            },
            Ok(Event::Text(t)) => {
                if in_text {
                    para.push_str(&t.unescape().unwrap_or_default());
                }
            }
            Ok(Event::End(e)) => match e.name().as_ref() {
                b"w:t" => in_text = false,
                b"w:p" => {
                    let line = para.trim();
                    if !line.is_empty() {
                        out.push_str(&line_prefix(style.as_deref(), is_list));
                        out.push_str(line);
                        out.push_str("\n\n");
                    }
                    para.clear();
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(e) => return Err(anyhow!("malformed document.xml: {e}")),
            _ => {}
        }
        buf.clear();
    }

    let out = out.trim();
    if out.is_empty() {
        return Err(anyhow!("the document appears to have no readable text"));
    }
    Ok(out.to_string())
}

/// The markdown prefix for a paragraph, given its Word paragraph-style id and
/// whether it carries list numbering. "Heading1".."Heading6" → `#`..`######`.
fn line_prefix(style: Option<&str>, is_list: bool) -> String {
    if let Some(s) = style {
        if let Some(rest) = s.strip_prefix("Heading") {
            let level: usize = rest.parse().unwrap_or(2).clamp(1, 6);
            return format!("{} ", "#".repeat(level));
        }
    }
    if is_list {
        return "- ".to_string();
    }
    String::new()
}

/// First matching attribute value on an element, unescaped to a String.
fn attr_val(e: &BytesStart, key: &[u8]) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == key)
        .and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()))
}

/// Pull the text layer out of a PDF.
fn pdf_to_text(bytes: &[u8]) -> Result<String> {
    let text = pdf_extract::extract_text_from_mem(bytes)
        .context("couldn't read the PDF's text")?;
    let text = text.trim();
    if text.is_empty() {
        return Err(anyhow!(
            "no text found — this looks like a scanned or image-only PDF (OCR isn't supported yet)"
        ));
    }
    Ok(text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Build a minimal in-memory .docx (a zip holding word/document.xml).
    fn make_docx(document_xml: &str) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zw.start_file("word/document.xml", opts).unwrap();
            zw.write_all(document_xml.as_bytes()).unwrap();
            zw.finish().unwrap();
        }
        buf
    }

    #[test]
    fn maps_headings_lists_and_text() {
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:body>
<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Title Here</w:t></w:r></w:p>
<w:p><w:pPr><w:pStyle w:val="Heading2"/></w:pPr><w:r><w:t>A Section</w:t></w:r></w:p>
<w:p><w:r><w:t>A plain </w:t></w:r><w:r><w:t>paragraph.</w:t></w:r></w:p>
<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>First bullet</w:t></w:r></w:p>
</w:body>
</w:document>"#;
        let md = docx_to_markdown(&make_docx(xml)).unwrap();
        assert!(md.contains("# Title Here"), "H1 not mapped: {md}");
        assert!(md.contains("## A Section"), "H2 not mapped: {md}");
        // Adjacent runs in one paragraph concatenate into a single line.
        assert!(md.contains("A plain paragraph."), "runs not joined: {md}");
        assert!(md.contains("- First bullet"), "list not mapped: {md}");
    }

    #[test]
    fn rejects_non_docx_bytes() {
        assert!(docx_to_markdown(b"not a zip").is_err());
    }
}
