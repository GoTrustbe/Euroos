//! EuroDoc — the Universal Document Model (ES-Core of EuroSuite).
//!
//! One tree for Writer (text), Calc (spreadsheet) and Impress (presentation). The
//! parsers (ES-IO: OOXML/ODF/PDF) build it; the apps render and edit it. This
//! crate provides the model, a **style registry with inheritance**, and derived
//! operations (text extraction, word/character statistics) — pure, host-tested
//! `no_std` logic without external dependencies.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod model;

use alloc::string::String;
use alloc::vec::Vec;
use model::{Block, Body, Cell, RunProperties};

pub use model::{
    Alignment, Color, DocumentKind, Metadata, Paragraph, ParagraphProperties, Run, SheetBody, Slide, Table,
};

/// A named style (can inherit from a parent style).
#[derive(Clone, PartialEq, Debug)]
pub struct Style {
    pub id: String,
    pub parent: Option<String>,
    pub run: RunProperties,
}

/// The style registry: named styles with inheritance.
#[derive(Default)]
pub struct StyleRegistry {
    styles: Vec<Style>,
}

impl StyleRegistry {
    pub fn new() -> StyleRegistry {
        StyleRegistry { styles: Vec::new() }
    }

    pub fn define(&mut self, id: &str, parent: Option<&str>, run: RunProperties) {
        let style = Style { id: String::from(id), parent: parent.map(String::from), run };
        if let Some(e) = self.styles.iter_mut().find(|s| s.id == style.id) {
            *e = style;
        } else {
            self.styles.push(style);
        }
    }

    fn get(&self, id: &str) -> Option<&Style> {
        self.styles.iter().find(|s| s.id == id)
    }

    /// Resolve the effective run formatting of a style by folding the parent
    /// chain (parent first, child overrides). Cycle-safe (depth limit).
    pub fn resolve(&self, id: &str) -> RunProperties {
        let mut chain: Vec<&Style> = Vec::new();
        let mut cur = self.get(id);
        let mut guard = 0;
        while let Some(s) = cur {
            chain.push(s);
            guard += 1;
            if guard > 32 {
                break; // cycle protection
            }
            cur = s.parent.as_deref().and_then(|p| self.get(p));
        }
        // Fold from parent (at the end) to child (at the front).
        let mut props = RunProperties::default();
        for s in chain.iter().rev() {
            props = props.merge(&s.run);
        }
        props
    }
}

/// A document: kind + metadata + body + styles.
pub struct Document {
    pub kind: DocumentKind,
    pub metadata: Metadata,
    pub body: Body,
    pub styles: StyleRegistry,
}

impl Document {
    pub fn writer() -> Document {
        Document {
            kind: DocumentKind::Writer,
            metadata: Metadata::default(),
            body: Body::Writer(Vec::new()),
            styles: StyleRegistry::new(),
        }
    }
    pub fn sheet() -> Document {
        Document {
            kind: DocumentKind::Sheet,
            metadata: Metadata::default(),
            body: Body::Sheet(SheetBody::default()),
            styles: StyleRegistry::new(),
        }
    }
    pub fn deck() -> Document {
        Document {
            kind: DocumentKind::Deck,
            metadata: Metadata::default(),
            body: Body::Deck(Vec::new()),
            styles: StyleRegistry::new(),
        }
    }

    /// The plain text of the whole document (for indexing, search, screen reader).
    pub fn plain_text(&self) -> String {
        let mut out = String::new();
        match &self.body {
            Body::Writer(blocks) => blocks_text(blocks, &mut out),
            Body::Sheet(sheet) => {
                let mut cells = sheet.cells.clone();
                cells.sort_by_key(|(r, c, _)| (*r, *c));
                for (i, (_, _, cell)) in cells.iter().enumerate() {
                    if i > 0 {
                        out.push('\t');
                    }
                    out.push_str(&cell_text(cell));
                }
            }
            Body::Deck(slides) => {
                for s in slides {
                    if !s.title.is_empty() {
                        out.push_str(&s.title);
                        out.push('\n');
                    }
                    blocks_text(&s.blocks, &mut out);
                }
            }
        }
        out
    }

    /// Word count of the whole document (whitespace-separated tokens).
    pub fn word_count(&self) -> usize {
        self.plain_text().split_whitespace().filter(|w| !w.is_empty()).count()
    }

    /// Character count (Unicode characters) of the whole document.
    pub fn char_count(&self) -> usize {
        self.plain_text().chars().filter(|c| !c.is_whitespace()).count()
    }

    /// The number of paragraphs (Writer) / cells (Sheet) / slides (Deck).
    pub fn element_count(&self) -> usize {
        match &self.body {
            Body::Writer(blocks) => blocks.iter().filter(|b| matches!(b, Block::Paragraph(_))).count(),
            Body::Sheet(s) => s.cells.iter().filter(|(_, _, c)| !matches!(c, Cell::Empty)).count(),
            Body::Deck(s) => s.len(),
        }
    }
}

fn blocks_text(blocks: &[Block], out: &mut String) {
    for b in blocks {
        match b {
            Block::Paragraph(p) => {
                out.push_str(&p.plain_text());
                out.push('\n');
            }
            Block::Table(t) => {
                for row in &t.rows {
                    for (i, cell) in row.iter().enumerate() {
                        if i > 0 {
                            out.push('\t');
                        }
                        for p in cell {
                            out.push_str(&p.plain_text());
                        }
                    }
                    out.push('\n');
                }
            }
            Block::Image { alt, .. } => {
                if !alt.is_empty() {
                    out.push('[');
                    out.push_str(alt);
                    out.push(']');
                    out.push('\n');
                }
            }
            Block::PageBreak | Block::HorizontalRule => out.push('\n'),
        }
    }
}

fn cell_text(c: &Cell) -> String {
    match c {
        Cell::Empty => String::new(),
        Cell::Text(s) | Cell::Formula(s) => s.clone(),
        Cell::Number { scaled, scale } => {
            // Compact rendering without floating point.
            if *scale == 0 {
                return alloc::format!("{scaled}");
            }
            let div = 10i64.pow(*scale as u32);
            alloc::format!("{}.{:0width$}", scaled / div, (scaled % div).abs(), width = *scale as usize)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::Cell;

    #[test]
    fn writer_text_and_stats() {
        let mut doc = Document::writer();
        if let Body::Writer(b) = &mut doc.body {
            b.push(Block::Paragraph(Paragraph::text("Hello world from EuroOS").styled("Heading1")));
            b.push(Block::Paragraph(Paragraph::new().run(Run::new("Bold").bold()).run(Run::new(" and normal"))));
        }
        assert_eq!(doc.plain_text(), "Hello world from EuroOS\nBold and normal\n");
        assert_eq!(doc.word_count(), 7);
        assert_eq!(doc.element_count(), 2);
    }

    #[test]
    fn style_inheritance() {
        let mut reg = StyleRegistry::new();
        let mut base = RunProperties::default();
        base.font_family = Some(String::from("EuroSans"));
        base.half_points = Some(24); // 12pt
        reg.define("Normal", None, base);
        let mut h1 = RunProperties::default();
        h1.bold = true;
        h1.half_points = Some(40); // 20pt overrides
        reg.define("Heading1", Some("Normal"), h1);

        let r = reg.resolve("Heading1");
        assert!(r.bold); // from Heading1
        assert_eq!(r.half_points, Some(40)); // child overrides parent
        assert_eq!(r.font_family.as_deref(), Some("EuroSans")); // inherited from Normal
    }

    #[test]
    fn style_cycle_safe() {
        let mut reg = StyleRegistry::new();
        reg.define("A", Some("B"), RunProperties::default());
        reg.define("B", Some("A"), RunProperties::default());
        let _ = reg.resolve("A"); // must not hang
    }

    #[test]
    fn sheet_text() {
        let mut doc = Document::sheet();
        if let Body::Sheet(s) = &mut doc.body {
            s.set(0, 0, Cell::Text(String::from("Revenue")));
            s.set(0, 1, Cell::Number { scaled: 123456, scale: 2 }); // 1234.56
            s.set(1, 0, Cell::Formula(String::from("=SUM(B1:B9)")));
        }
        assert_eq!(doc.plain_text(), "Revenue\t1234.56\t=SUM(B1:B9)");
        assert_eq!(doc.element_count(), 3);
    }

    #[test]
    fn deck_text() {
        let mut doc = Document::deck();
        if let Body::Deck(s) = &mut doc.body {
            s.push(Slide { title: String::from("Welcome"), blocks: alloc::vec![Block::Paragraph(Paragraph::text("First slide"))] });
            s.push(Slide { title: String::from("End"), blocks: alloc::vec![] });
        }
        assert_eq!(doc.element_count(), 2);
        assert!(doc.plain_text().contains("Welcome"));
        assert!(doc.plain_text().contains("First slide"));
    }

    #[test]
    fn sheet_cell_overwrite() {
        let mut s = SheetBody::default();
        s.set(0, 0, Cell::Text(String::from("a")));
        s.set(0, 0, Cell::Text(String::from("b")));
        assert_eq!(s.get(0, 0), Cell::Text(String::from("b")));
        assert_eq!(s.cells.len(), 1);
    }
}
