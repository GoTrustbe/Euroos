//! EuroDocIO — document I/O for EuroSuite (ES-IO).
//!
//! A sovereign office suite reads and writes the formats that Europe uses —
//! **OOXML** (`.docx`/…) and **OpenDocument** (`.odt`/…) — and exports to HTML,
//! all to/from the single [`eurodoc`] UDM. This crate contains the in-house XML parser
//! plus the format bindings, `no_std` and host-tested. The **ZIP container
//! (DEFLATE)** now lives here too ([`zip`] + [`docx`], on [`euroflate`]), so a
//! real `.docx` opens and saves end-to-end — not just a pre-extracted
//! `document.xml`.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod docx;
pub mod html;
pub mod odf;
pub mod ooxml;
pub mod xml;
pub mod zip;

#[cfg(test)]
mod tests {
    use eurodoc::model::Block;

    /// OOXML → UDM → HTML: a small end-to-end pipeline.
    #[test]
    fn docx_to_html_pipeline() {
        let docx = r#"<w:body>
            <w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>EuroSuite</w:t></w:r></w:p>
            <w:p><w:r><w:rPr><w:b/></w:rPr><w:t>Vet</w:t></w:r><w:r><w:t> en gewoon.</w:t></w:r></w:p>
        </w:body>"#;
        let blocks = super::ooxml::parse_body(docx);
        let html = super::html::blocks_to_html(&blocks);
        assert!(html.contains("<h1>EuroSuite</h1>"));
        assert!(html.contains("<strong>Vet</strong>"));
        assert!(html.contains(" en gewoon."));
    }

    /// OOXML and ODF that mean the same thing yield the same plain text.
    #[test]
    fn ooxml_and_odf_agree_on_text() {
        let docx = r#"<w:body><w:p><w:r><w:t>Hallo</w:t></w:r></w:p></w:body>"#;
        let odt = r#"<office:text><text:p>Hallo</text:p></office:text>"#;
        let a = super::ooxml::parse_body(docx);
        let b = super::odf::parse_body(odt);
        let text = |blocks: &[Block]| {
            blocks
                .iter()
                .filter_map(|b| if let Block::Paragraph(p) = b { Some(p.plain_text()) } else { None })
                .collect::<alloc::vec::Vec<_>>()
                .join("\n")
        };
        assert_eq!(text(&a), text(&b));
    }
}
