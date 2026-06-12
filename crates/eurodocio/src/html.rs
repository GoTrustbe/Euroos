//! HTML-export van het EuroDoc-UDM — voor preview, web-publicatie en de schermlezer.

use crate::xml::encode_entities;
use alloc::string::String;
use eurodoc::model::{Block, Paragraph, Run};

/// Exporteer een lijst blokken naar HTML.
pub fn blocks_to_html(blocks: &[Block]) -> String {
    let mut s = String::new();
    for b in blocks {
        match b {
            Block::Paragraph(p) => para_html(p, &mut s),
            Block::Table(t) => {
                s.push_str("<table>");
                for row in &t.rows {
                    s.push_str("<tr>");
                    for cell in row {
                        s.push_str("<td>");
                        for p in cell {
                            para_html(p, &mut s);
                        }
                        s.push_str("</td>");
                    }
                    s.push_str("</tr>");
                }
                s.push_str("</table>");
            }
            Block::Image { alt, width, height } => {
                s.push_str("<img alt=\"");
                s.push_str(&encode_entities(alt));
                s.push_str(&alloc::format!("\" width=\"{width}\" height=\"{height}\"/>"));
            }
            Block::PageBreak => s.push_str("<hr class=\"page-break\"/>"),
            Block::HorizontalRule => s.push_str("<hr/>"),
        }
    }
    s
}

fn para_html(p: &Paragraph, s: &mut String) {
    // Kies de tag op basis van de stijl: HeadingN → hN, anders p.
    let (open, close) = match p.props.style_id.as_deref() {
        Some(style) if style.starts_with("Heading") => {
            let level = style.trim_start_matches("Heading").parse::<u8>().unwrap_or(1).clamp(1, 6);
            match level {
                1 => ("<h1>", "</h1>"),
                2 => ("<h2>", "</h2>"),
                3 => ("<h3>", "</h3>"),
                4 => ("<h4>", "</h4>"),
                5 => ("<h5>", "</h5>"),
                _ => ("<h6>", "</h6>"),
            }
        }
        _ => ("<p>", "</p>"),
    };
    s.push_str(open);
    for r in &p.runs {
        run_html(r, s);
    }
    s.push_str(close);
}

fn run_html(r: &Run, s: &mut String) {
    let mut open = String::new();
    let mut close = String::new();
    if r.props.bold {
        open.push_str("<strong>");
        close.insert_str(0, "</strong>");
    }
    if r.props.italic {
        open.push_str("<em>");
        close.insert_str(0, "</em>");
    }
    if r.props.underline {
        open.push_str("<u>");
        close.insert_str(0, "</u>");
    }
    if r.props.strikethrough {
        open.push_str("<s>");
        close.insert_str(0, "</s>");
    }
    s.push_str(&open);
    s.push_str(&encode_entities(&r.text));
    s.push_str(&close);
}

#[cfg(test)]
mod tests {
    use super::*;
    use eurodoc::model::{Paragraph, Run};

    #[test]
    fn headings_and_runs() {
        let blocks = alloc::vec![
            Block::Paragraph(Paragraph::text("Titel").styled("Heading1")),
            Block::Paragraph(Paragraph::new().run(Run::new("vet").bold()).run(Run::new(" cursief").italic())),
        ];
        let html = blocks_to_html(&blocks);
        assert_eq!(html, "<h1>Titel</h1><p><strong>vet</strong><em> cursief</em></p>");
    }

    #[test]
    fn escapes_text() {
        let blocks = alloc::vec![Block::Paragraph(Paragraph::text("a < b & c"))];
        assert_eq!(blocks_to_html(&blocks), "<p>a &lt; b &amp; c</p>");
    }
}
