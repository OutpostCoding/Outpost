// ── surface: backend ──────────────────────────────────
// Markdown → themed .docx (hand-rolled OOXML; CV theme: A4, Calibri, navy 1F3A5F).
// Exports: markdown_to_docx, export_docx · Related: commands.rs, hooks/useFileManager.ts

//! Document generation — markdown → a themed `.docx`. The theme is the locked
//! "standard" reverse-engineered from the project's own design: A4, Calibri, deep-navy
//! (`1F3A5F`) name/section-headings/rules + grey (`555555`) subtitle, black body.
//!
//! We hand-roll the OOXML (a `.docx` is a zip of XML) rather than depend on a
//! docx library: it gives exact control over the theme's paragraph border rules
//! and letter-spacing, and needs no new crate. Bullets are rendered as a literal
//! mark + tab (with a hanging indent) so we can skip the numbering-relationship
//! machinery entirely — visually identical, far less package plumbing.
//!
//! Markdown → theme mapping (the tool description teaches the model this):
//!   `# H1`      → title/name (20pt bold navy). The first plain line right after
//!                 the first H1 becomes the grey subtitle under a thick navy rule.
//!   `## H2`     → UPPERCASE section heading (12pt bold navy, letter-spaced) under
//!                 a thin navy rule.
//!   `### H3`+   → role/subheading (11pt bold).
//!   `- ` / `* ` → bullet (10.5pt).
//!   `> `        → blockquote (indented, muted, navy left rule).
//!   `---`       → thematic break (skipped — the heading rules already structure).
//!   other       → body paragraph (10.5pt). `**bold**` and `*italic*` are honoured.

use anyhow::Result;
use std::io::Write;
use tauri_plugin_dialog::DialogExt;

const ACCENT: &str = "1F3A5F";
const MUTED: &str = "555555";

/// Convert markdown to a themed `.docx`, returned as the raw file bytes.
pub fn markdown_to_docx(markdown: &str) -> Result<Vec<u8>> {
    let document = build_document_xml(markdown);
    pack(&[
        ("[Content_Types].xml", CONTENT_TYPES.to_string()),
        ("_rels/.rels", ROOT_RELS.to_string()),
        ("word/_rels/document.xml.rels", DOC_RELS.to_string()),
        ("word/styles.xml", STYLES.to_string()),
        ("word/document.xml", document),
    ])
}

/// Generate a `.docx` from markdown and let the user save it anywhere via a
/// native Save dialog, then open it. Returns the saved path, or `None` if the
/// user cancelled. This is the user-driven, deterministic counterpart to reading
/// documents in — no model involved; it just converts the note being viewed.
#[tauri::command]
pub async fn export_docx(
    app: tauri::AppHandle,
    markdown: String,
    default_name: String,
) -> Result<Option<String>, String> {
    let bytes = markdown_to_docx(&markdown).map_err(|e| e.to_string())?;

    // Native Save dialog (async): the callback fires with the chosen path once the
    // user picks a name/location, which we relay back over a oneshot channel.
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .add_filter("Word document", &["docx"])
        .set_file_name(&default_name)
        .save_file(move |path| {
            let _ = tx.send(path);
        });
    let chosen = rx
        .await
        .map_err(|_| "the save dialog closed unexpectedly".to_string())?;
    let Some(file_path) = chosen else {
        return Ok(None); // the user cancelled
    };

    let path = file_path.into_path().map_err(|e| e.to_string())?;
    std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
    let _ = crate::tools::open_in_default_app(&path);
    Ok(Some(path.display().to_string()))
}

/// Build `word/document.xml` from the markdown, applying the standard theme.
fn build_document_xml(markdown: &str) -> String {
    let mut body = String::new();
    let mut seen_h1 = false;
    let mut subtitle_pending = false;

    for raw in markdown.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue; // blank lines are just separators
        }
        if is_thematic_break(line) {
            subtitle_pending = false;
            continue; // '---' etc. — the heading rules already structure the doc
        }

        if let Some((level, text)) = heading(line) {
            match level {
                1 => {
                    let first = !seen_h1;
                    seen_h1 = true;
                    body.push_str(&title_para(text));
                    subtitle_pending = first; // a subtitle may follow the first H1
                }
                2 => {
                    subtitle_pending = false;
                    body.push_str(&section_para(text));
                }
                _ => {
                    subtitle_pending = false;
                    body.push_str(&role_para(text));
                }
            }
        } else if let Some(text) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
            subtitle_pending = false;
            body.push_str(&bullet_para(text));
        } else if let Some(text) = line.strip_prefix("> ").or_else(|| line.strip_prefix(">")) {
            subtitle_pending = false;
            body.push_str(&quote_para(text));
        } else if subtitle_pending {
            subtitle_pending = false;
            body.push_str(&subtitle_para(line));
        } else {
            body.push_str(&body_para(line));
        }
    }

    format!(
        "{XML_DECL}<w:document xmlns:w=\"{W}\"><w:body>{body}{SECT}</w:body></w:document>"
    )
}

// --- paragraph builders (each returns one <w:p>) ----------------------------

/// `# ` → name/title: 20pt bold navy.
fn title_para(text: &str) -> String {
    para(
        "<w:spacing w:after=\"20\"/>",
        &run("<w:b/><w:bCs/><w:color w:val=\"1F3A5F\"/><w:sz w:val=\"40\"/><w:szCs w:val=\"40\"/>", text),
    )
}

/// The line after the first H1 → grey subtitle under a thick navy rule.
fn subtitle_para(text: &str) -> String {
    para(
        &format!("<w:pBdr><w:bottom w:val=\"single\" w:sz=\"12\" w:space=\"4\" w:color=\"{ACCENT}\"/></w:pBdr><w:spacing w:after=\"120\"/>"),
        &inline_runs(text, &format!("<w:color w:val=\"{MUTED}\"/><w:sz w:val=\"20\"/><w:szCs w:val=\"20\"/>")),
    )
}

/// `## ` → UPPERCASE section heading, 12pt bold navy, letter-spaced, thin rule.
fn section_para(text: &str) -> String {
    para(
        &format!("<w:pBdr><w:bottom w:val=\"single\" w:sz=\"8\" w:space=\"2\" w:color=\"{ACCENT}\"/></w:pBdr><w:spacing w:before=\"240\" w:after=\"100\"/>"),
        &run(
            &format!("<w:b/><w:bCs/><w:color w:val=\"{ACCENT}\"/><w:spacing w:val=\"20\"/><w:sz w:val=\"24\"/><w:szCs w:val=\"24\"/>"),
            &text.to_uppercase(),
        ),
    )
}

/// `### `+ → role/subheading: 11pt bold black.
fn role_para(text: &str) -> String {
    para(
        "<w:spacing w:before=\"140\" w:after=\"20\"/>",
        &inline_runs(text, "<w:b/><w:bCs/><w:sz w:val=\"22\"/><w:szCs w:val=\"22\"/>"),
    )
}

/// `- ` → bullet: a literal mark + tab, hanging indent, 10.5pt body.
fn bullet_para(text: &str) -> String {
    let marker = "<w:r><w:rPr><w:sz w:val=\"21\"/><w:szCs w:val=\"21\"/></w:rPr><w:t xml:space=\"preserve\">\u{2022}</w:t><w:tab/></w:r>";
    para(
        "<w:spacing w:after=\"60\"/><w:ind w:left=\"360\" w:hanging=\"220\"/>",
        &format!("{marker}{}", inline_runs(text, "<w:sz w:val=\"21\"/><w:szCs w:val=\"21\"/>")),
    )
}

/// Plain paragraph → body: 10.5pt (inherits), with inline `**bold**`/`*italic*`.
fn body_para(text: &str) -> String {
    para("<w:spacing w:after=\"80\"/>", &inline_runs(text, ""))
}

/// `> ` → blockquote: indented, muted, with a navy left rule.
fn quote_para(text: &str) -> String {
    para(
        &format!("<w:pBdr><w:left w:val=\"single\" w:sz=\"18\" w:space=\"8\" w:color=\"{ACCENT}\"/></w:pBdr><w:spacing w:after=\"80\"/><w:ind w:left=\"280\"/>"),
        &inline_runs(text, &format!("<w:color w:val=\"{MUTED}\"/>")),
    )
}

// --- run / inline helpers ---------------------------------------------------

fn para(ppr: &str, runs: &str) -> String {
    format!("<w:p><w:pPr>{ppr}</w:pPr>{runs}</w:p>")
}

fn run(rpr: &str, text: &str) -> String {
    format!("<w:r><w:rPr>{rpr}</w:rPr><w:t xml:space=\"preserve\">{}</w:t></w:r>", esc(text))
}

/// Emit runs for inline markdown, honouring `**bold**` and `*italic*` (and their
/// combination), all sharing the given base run-properties. A small toggle
/// tokenizer: `**` flips bold, a lone `*` flips italic.
fn inline_runs(text: &str, base_rpr: &str) -> String {
    let mut out = String::new();
    let mut buf = String::new();
    let mut bold = false;
    let mut italic = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '*' {
            if chars.peek() == Some(&'*') {
                chars.next(); // consume the second '*'
                flush_run(&mut out, &mut buf, base_rpr, bold, italic);
                bold = !bold;
            } else {
                flush_run(&mut out, &mut buf, base_rpr, bold, italic);
                italic = !italic;
            }
        } else {
            buf.push(c);
        }
    }
    flush_run(&mut out, &mut buf, base_rpr, bold, italic);
    if out.is_empty() {
        // A line that was only delimiters — keep the raw text so nothing is lost.
        out.push_str(&run(base_rpr, text));
    }
    out
}

/// Emit the buffered text as one run with bold/italic applied over the base.
fn flush_run(out: &mut String, buf: &mut String, base_rpr: &str, bold: bool, italic: bool) {
    if buf.is_empty() {
        return;
    }
    let mut rpr = String::new();
    if bold {
        rpr.push_str("<w:b/><w:bCs/>");
    }
    if italic {
        rpr.push_str("<w:i/><w:iCs/>");
    }
    rpr.push_str(base_rpr);
    out.push_str(&run(&rpr, buf));
    buf.clear();
}

/// Heading level + text if the line is an ATX heading (`#`..`######` + space).
fn heading(line: &str) -> Option<(usize, &str)> {
    let hashes = line.bytes().take_while(|&b| b == b'#').count();
    if (1..=6).contains(&hashes) && line.as_bytes().get(hashes) == Some(&b' ') {
        Some((hashes, line[hashes + 1..].trim()))
    } else {
        None
    }
}

/// True if the line is a markdown thematic break (`---`, `***`, `___`).
fn is_thematic_break(line: &str) -> bool {
    let t = line.trim();
    t.len() >= 3
        && (t.chars().all(|c| c == '-')
            || t.chars().all(|c| c == '*')
            || t.chars().all(|c| c == '_'))
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Zip the OOXML parts into `.docx` bytes (deflate).
fn pack(files: &[(&str, String)]) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    {
        let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, content) in files {
            zw.start_file(*name, opts)?;
            zw.write_all(content.as_bytes())?;
        }
        zw.finish()?;
    }
    Ok(buf)
}

// --- fixed package parts ----------------------------------------------------

const XML_DECL: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>";
const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
// A4 (11906 x 16838) with the project's CV margins: 0.5" top/bottom (720), 0.625" sides (900).
const SECT: &str = "<w:sectPr><w:pgSz w:w=\"11906\" w:h=\"16838\"/><w:pgMar w:top=\"720\" w:right=\"900\" w:bottom=\"720\" w:left=\"900\" w:header=\"708\" w:footer=\"708\" w:gutter=\"0\"/></w:sectPr>";

const CONTENT_TYPES: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
<Default Extension=\"xml\" ContentType=\"application/xml\"/>\
<Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>\
<Override PartName=\"/word/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml\"/>\
</Types>";

const ROOT_RELS: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/>\
</Relationships>";

const DOC_RELS: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/>\
</Relationships>";

// Calibri as the document default at 10.5pt (matches the CV's body), plus the
// required default Normal style. Everything else is formatted inline per-run.
const STYLES: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<w:styles xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
<w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii=\"Calibri\" w:hAnsi=\"Calibri\" w:cs=\"Calibri\"/><w:sz w:val=\"21\"/><w:szCs w:val=\"21\"/></w:rPr></w:rPrDefault></w:docDefaults>\
<w:style w:type=\"paragraph\" w:default=\"1\" w:styleId=\"Normal\"><w:name w:val=\"Normal\"/><w:qFormat/></w:style>\
</w:styles>";

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn document_xml(bytes: &[u8]) -> String {
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut s = String::new();
        zip.by_name("word/document.xml").unwrap().read_to_string(&mut s).unwrap();
        s
    }

    #[test]
    fn generates_valid_themed_docx() {
        let md = "# Jane Doe\nOne-line subtitle\n\n## Experience\n### Senior Engineer\nAcme Corp | 2020\n- Did a **great** thing\n\nA plain paragraph.";
        let bytes = markdown_to_docx(md).unwrap();

        // It's a real zip with the required parts.
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(&bytes)).unwrap();
        for part in ["[Content_Types].xml", "_rels/.rels", "word/document.xml", "word/styles.xml"] {
            assert!(zip.by_name(part).is_ok(), "missing part {part}");
        }

        let doc = document_xml(&bytes);
        // Theme markers.
        assert!(doc.contains("Jane Doe") && doc.contains("w:sz w:val=\"40\""), "name style: {doc}");
        assert!(doc.contains("EXPERIENCE"), "section not uppercased: {doc}");
        assert!(doc.contains(&format!("w:color w:val=\"{ACCENT}\"")), "navy accent missing");
        assert!(doc.contains("w:pBdr"), "rule/border missing");
        assert!(doc.contains("A4") == false); // sanity: no stray text
        assert!(doc.contains("w:pgSz w:w=\"11906\""), "not A4");
        // Inline bold split produced a bold run around "great".
        assert!(doc.contains(">great<"), "bold text lost: {doc}");
    }

    #[test]
    fn escapes_xml_special_chars() {
        let bytes = markdown_to_docx("R&D <tags> & more").unwrap();
        let doc = document_xml(&bytes);
        assert!(doc.contains("R&amp;D &lt;tags&gt; &amp; more"), "not escaped: {doc}");
    }

    #[test]
    fn handles_italic_blockquote_and_breaks() {
        let md = "# T\n**Sub:** x\n\n---\n\n> *quoted* bit\n\nplain *italic* and **bold**";
        let doc = document_xml(&markdown_to_docx(md).unwrap());
        assert!(doc.contains("<w:i/>"), "italic run missing: {doc}");
        assert!(doc.contains("w:left w:val=\"single\""), "blockquote rule missing: {doc}");
        assert!(doc.contains(">quoted<"), "quote text lost: {doc}");
        // Markers must not leak as literal text, and the break must be skipped.
        assert!(!doc.contains('*'), "raw asterisk leaked: {doc}");
        assert!(!doc.contains(">---<") && !doc.contains(">-<"), "thematic break leaked: {doc}");
        // The subtitle's **bold** was applied, not shown literally.
        assert!(doc.contains(">Sub:<"), "subtitle bold text lost: {doc}");
    }
}
