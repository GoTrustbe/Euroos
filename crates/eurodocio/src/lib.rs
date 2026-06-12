//! EuroDocIO — document-I/O voor EuroSuite (ES-IO).
//!
//! Een soeverein kantoorpakket leest en schrijft de formaten die Europa gebruikt —
//! **OOXML** (`.docx`/…) en **OpenDocument** (`.odt`/…) — én exporteert naar HTML,
//! allemaal naar/uit het ene [`eurodoc`]-UDM. Dit crate bevat de eigen XML-parser
//! plus de format-bindingen, `no_std` en host-getest. De ZIP-container (deflate) is
//! een aparte, dunne laag die de kernel/`eupkg` levert; hier werken we op de
//! uitgepakte XML-onderdelen.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod html;
pub mod odf;
pub mod ooxml;
pub mod xml;

#[cfg(test)]
mod tests {
    use eurodoc::model::Block;

    /// OOXML → UDM → HTML: een kleine end-to-end-pijplijn.
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

    /// OOXML en ODF die hetzelfde betekenen leveren dezelfde platte tekst.
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
