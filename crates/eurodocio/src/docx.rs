//! Real `.docx` open/save — the ZIP-container ([`crate::zip`]) + the OOXML body
//! ([`crate::ooxml`]) joined so EuroSuite Writer opens and saves documents a real
//! word processor produced, not just a pre-extracted `document.xml`.
//!
//! A `.docx` is a ZIP with (at least) `[Content_Types].xml`, `_rels/.rels`,
//! `word/_rels/document.xml.rels` and `word/document.xml`. We read the body from
//! `word/document.xml` and, on save, emit a minimal-but-valid package that
//! Word/LibreOffice open.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use eurodoc::model::Block;

use crate::zip::{self, ZipEntry, ZipError};

/// Open a `.docx` byte buffer → the document body as EuroDoc blocks.
pub fn open(docx: &[u8]) -> Result<Vec<Block>, ZipError> {
    let xml = zip::read_entry(docx, "word/document.xml")?;
    let text = core::str::from_utf8(&xml).map_err(|_| ZipError::Inflate)?;
    Ok(crate::ooxml::parse_body(text))
}

const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;

const ROOT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;

const DOC_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"></Relationships>"#;

/// Save EuroDoc blocks → a valid `.docx` byte buffer (real tools open it).
pub fn save(blocks: &[Block]) -> Vec<u8> {
    let body = crate::ooxml::write_body(blocks);
    let document = alloc::format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}</w:body></w:document>"#
    );
    let entries = alloc::vec![
        ZipEntry { name: "[Content_Types].xml".to_string(), data: CONTENT_TYPES.as_bytes().to_vec() },
        ZipEntry { name: "_rels/.rels".to_string(), data: ROOT_RELS.as_bytes().to_vec() },
        ZipEntry { name: "word/_rels/document.xml.rels".to_string(), data: DOC_RELS.as_bytes().to_vec() },
        ZipEntry { name: "word/document.xml".to_string(), data: document.into_bytes() },
    ];
    zip::write(&entries)
}

/// Concatenate all paragraph text of a body (for a quick word/character check).
pub fn plain_text(blocks: &[Block]) -> String {
    let mut s = String::new();
    for b in blocks {
        if let Block::Paragraph(p) = b {
            for r in &p.runs {
                s.push_str(&r.text);
            }
            s.push('\n');
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_open_roundtrip() {
        let blocks = alloc::vec![
            Block::Paragraph(eurodoc::model::Paragraph::text("Sovereignty by design.")),
            Block::Paragraph(eurodoc::model::Paragraph::text("A second paragraph.")),
        ];
        let docx = save(&blocks);
        // It is a real ZIP with the expected parts.
        let names: Vec<String> = zip::read(&docx).unwrap().into_iter().map(|e| e.name).collect();
        assert!(names.iter().any(|n| n == "word/document.xml"));
        assert!(names.iter().any(|n| n == "[Content_Types].xml"));
        // Round-trips through open().
        let back = open(&docx).unwrap();
        assert_eq!(plain_text(&back).trim(), "Sovereignty by design.\nA second paragraph.");
    }
}
