//! OOXML WordprocessingML (`.docx` `word/document.xml`) ↔ het EuroDoc-UDM.
//!
//! Werkt op de (al uitgepakte) `document.xml`-inhoud; de ZIP-container is een aparte
//! laag. Leest paragrafen (`w:p`), runs (`w:r`) met opmaak (`w:b`/`w:i`/`w:u`),
//! tekst (`w:t`) en paragraaf-stijlen (`w:pStyle`); schrijft hetzelfde terug.

use crate::xml::{encode_entities, parse, Event};
use alloc::string::String;
use alloc::vec::Vec;
use eurodoc::model::{Block, Paragraph, ParagraphProperties, Run, RunProperties};

/// Parse de inhoud van een `word/document.xml` naar een lijst blokken.
pub fn parse_body(xml: &str) -> Vec<Block> {
    let events = parse(xml);
    let mut blocks = Vec::new();
    let mut para: Option<Paragraph> = None;
    let mut run: Option<Run> = None;
    let mut in_rpr = false;

    for ev in &events {
        match ev {
            Event::Open { name, attrs } => match name.as_str() {
                "w:p" => para = Some(Paragraph { props: ParagraphProperties::default(), runs: Vec::new() }),
                "w:pStyle" => {
                    if let (Some(p), Some(val)) = (para.as_mut(), attr(attrs, "w:val")) {
                        p.props.style_id = Some(val);
                    }
                }
                "w:r" => run = Some(Run { text: String::new(), props: RunProperties::default() }),
                "w:rPr" => in_rpr = true,
                "w:b" if in_rpr => set_run(&mut run, |p| p.bold = !is_false(attrs)),
                "w:i" if in_rpr => set_run(&mut run, |p| p.italic = !is_false(attrs)),
                "w:u" if in_rpr => set_run(&mut run, |p| p.underline = true),
                "w:strike" if in_rpr => set_run(&mut run, |p| p.strikethrough = true),
                _ => {}
            },
            Event::Text(t) => {
                // Tekst telt alleen binnen een <w:t> (dus binnen een run, niet in rPr).
                if let Some(r) = run.as_mut() {
                    if !in_rpr {
                        r.text.push_str(t);
                    }
                }
            }
            Event::Close { name } => match name.as_str() {
                "w:rPr" => in_rpr = false,
                "w:r" => {
                    if let (Some(p), Some(r)) = (para.as_mut(), run.take()) {
                        p.runs.push(r);
                    }
                }
                "w:p" => {
                    if let Some(p) = para.take() {
                        blocks.push(Block::Paragraph(p));
                    }
                }
                _ => {}
            },
        }
    }
    blocks
}

fn attr(attrs: &[(String, String)], key: &str) -> Option<String> {
    attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
}

/// `w:val="false"`/`"0"` betekent de toggle uít.
fn is_false(attrs: &[(String, String)]) -> bool {
    matches!(attr(attrs, "w:val").as_deref(), Some("false") | Some("0"))
}

fn set_run(run: &mut Option<Run>, f: impl FnOnce(&mut RunProperties)) {
    if let Some(r) = run.as_mut() {
        f(&mut r.props);
    }
}

/// Schrijf een lijst blokken naar `word/document.xml`-inhoud (WordprocessingML).
pub fn write_body(blocks: &[Block]) -> String {
    let mut s = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body>",
    );
    for b in blocks {
        if let Block::Paragraph(p) = b {
            s.push_str("<w:p>");
            if let Some(style) = &p.props.style_id {
                s.push_str("<w:pPr><w:pStyle w:val=\"");
                s.push_str(&encode_entities(style));
                s.push_str("\"/></w:pPr>");
            }
            for r in &p.runs {
                s.push_str("<w:r>");
                let rpr = run_props_xml(&r.props);
                if !rpr.is_empty() {
                    s.push_str("<w:rPr>");
                    s.push_str(&rpr);
                    s.push_str("</w:rPr>");
                }
                s.push_str("<w:t xml:space=\"preserve\">");
                s.push_str(&encode_entities(&r.text));
                s.push_str("</w:t></w:r>");
            }
            s.push_str("</w:p>");
        }
    }
    s.push_str("</w:body></w:document>");
    s
}

fn run_props_xml(p: &RunProperties) -> String {
    let mut s = String::new();
    if p.bold {
        s.push_str("<w:b/>");
    }
    if p.italic {
        s.push_str("<w:i/>");
    }
    if p.underline {
        s.push_str("<w:u w:val=\"single\"/>");
    }
    if p.strikethrough {
        s.push_str("<w:strike/>");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r#"<w:document><w:body>
        <w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Titel</w:t></w:r></w:p>
        <w:p><w:r><w:rPr><w:b/></w:rPr><w:t>Vet</w:t></w:r><w:r><w:t> en gewoon</w:t></w:r></w:p>
    </w:body></w:document>"#;

    #[test]
    fn parses_paragraphs_runs_styles() {
        let blocks = parse_body(DOC);
        assert_eq!(blocks.len(), 2);
        if let Block::Paragraph(p) = &blocks[0] {
            assert_eq!(p.props.style_id.as_deref(), Some("Heading1"));
            assert_eq!(p.plain_text(), "Titel");
        } else {
            panic!();
        }
        if let Block::Paragraph(p) = &blocks[1] {
            assert_eq!(p.runs.len(), 2);
            assert!(p.runs[0].props.bold);
            assert!(!p.runs[1].props.bold);
            assert_eq!(p.plain_text(), "Vet en gewoon");
        } else {
            panic!();
        }
    }

    #[test]
    fn write_then_parse_roundtrip() {
        let blocks = parse_body(DOC);
        let xml = write_body(&blocks);
        let again = parse_body(&xml);
        assert_eq!(again.len(), 2);
        if let Block::Paragraph(p) = &again[1] {
            assert!(p.runs[0].props.bold);
            assert_eq!(p.plain_text(), "Vet en gewoon");
        } else {
            panic!();
        }
    }

    #[test]
    fn escapes_special_chars() {
        let blocks = parse_body(r#"<w:body><w:p><w:r><w:t>a &amp; b &lt;x&gt;</w:t></w:r></w:p></w:body>"#);
        if let Block::Paragraph(p) = &blocks[0] {
            assert_eq!(p.plain_text(), "a & b <x>");
        }
        let xml = write_body(&blocks);
        assert!(xml.contains("a &amp; b &lt;x&gt;"));
    }
}
