//! OpenDocument Text (`.odt` `content.xml`) → the EuroDoc UDM. Reads `text:h`
//! (headings), `text:p` (paragraphs) and `text:span` (runs, bold detection via style name).
//! Works on the unpacked `content.xml`; the ZIP container is a separate layer.

use crate::xml::{parse, Event};
use alloc::string::String;
use alloc::vec::Vec;
use eurodoc::model::{Block, Paragraph, Run, RunProperties};

/// Parse an ODF `content.xml` text body into blocks.
pub fn parse_body(xml: &str) -> Vec<Block> {
    let events = parse(xml);
    let mut blocks = Vec::new();
    let mut para: Option<Paragraph> = None;
    let mut span_bold = false;
    let mut depth_in_span = 0u32;

    for ev in &events {
        match ev {
            Event::Open { name, attrs } => match name.as_str() {
                "text:p" => para = Some(Paragraph::default()),
                "text:h" => {
                    let mut p = Paragraph::default();
                    // A heading gets a Heading style (level from text:outline-level).
                    let level = attr(attrs, "text:outline-level").and_then(|v| v.parse::<u8>().ok()).unwrap_or(1);
                    p.props.style_id = Some(alloc::format!("Heading{level}"));
                    para = Some(p);
                }
                "text:span" => {
                    depth_in_span += 1;
                    // Bold detection on the style name (best-effort; full style table = ES-IO extension).
                    let style = attr(attrs, "text:style-name").unwrap_or_default().to_ascii_lowercase();
                    span_bold = style.contains("bold") || style.contains("vet") || style.contains("strong");
                }
                _ => {}
            },
            Event::Text(t) => {
                if let Some(p) = para.as_mut() {
                    let mut props = RunProperties::default();
                    if depth_in_span > 0 {
                        props.bold = span_bold;
                    }
                    p.runs.push(Run { text: t.clone(), props });
                }
            }
            Event::Close { name } => match name.as_str() {
                "text:span" => {
                    depth_in_span = depth_in_span.saturating_sub(1);
                    if depth_in_span == 0 {
                        span_bold = false;
                    }
                }
                "text:p" | "text:h" => {
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

#[cfg(test)]
mod tests {
    use super::*;

    const ODT: &str = r#"<office:text>
        <text:h text:outline-level="1">Titel</text:h>
        <text:p>Gewoon <text:span text:style-name="Bold_20_Text">vet</text:span> einde</text:p>
    </office:text>"#;

    #[test]
    fn parses_headings_and_spans() {
        let blocks = parse_body(ODT);
        assert_eq!(blocks.len(), 2);
        if let Block::Paragraph(h) = &blocks[0] {
            assert_eq!(h.props.style_id.as_deref(), Some("Heading1"));
            assert_eq!(h.plain_text(), "Titel");
        } else {
            panic!();
        }
        if let Block::Paragraph(p) = &blocks[1] {
            assert_eq!(p.plain_text(), "Gewoon vet einde");
            // The middle run (inside text:span "Bold...") is bold.
            let bold_run = p.runs.iter().find(|r| r.text == "vet").unwrap();
            assert!(bold_run.props.bold);
            let plain_run = p.runs.iter().find(|r| r.text.contains("Gewoon")).unwrap();
            assert!(!plain_run.props.bold);
        } else {
            panic!();
        }
    }
}
