//! EuroDoc — het Universeel Document Model (ES-Core van EuroSuite).
//!
//! Eén boom voor Writer (tekst), Calc (rekenblad) en Impress (presentatie). De
//! parsers (ES-IO: OOXML/ODF/PDF) bouwen 'm; de apps renderen + bewerken 'm. Dit
//! crate levert het model, een **stijlregister met overerving**, en afgeleide
//! bewerkingen (tekstextractie, woord-/tekenstatistiek) — pure, host-geteste
//! `no_std`-logica zonder externe afhankelijkheden.

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

/// Een benoemde stijl (kan van een ouder-stijl erven).
#[derive(Clone, PartialEq, Debug)]
pub struct Style {
    pub id: String,
    pub parent: Option<String>,
    pub run: RunProperties,
}

/// Het stijlregister: benoemde stijlen met overerving.
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

    /// Los de effectieve run-opmaak van een stijl op door de ouderketen samen te
    /// vouwen (ouder eerst, kind overschrijft). Cyclus-veilig (diepte-limiet).
    pub fn resolve(&self, id: &str) -> RunProperties {
        let mut chain: Vec<&Style> = Vec::new();
        let mut cur = self.get(id);
        let mut guard = 0;
        while let Some(s) = cur {
            chain.push(s);
            guard += 1;
            if guard > 32 {
                break; // cyclus-bescherming
            }
            cur = s.parent.as_deref().and_then(|p| self.get(p));
        }
        // Van ouder (achteraan) naar kind (vooraan) samenvouwen.
        let mut props = RunProperties::default();
        for s in chain.iter().rev() {
            props = props.merge(&s.run);
        }
        props
    }
}

/// Een document: soort + metadata + body + stijlen.
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

    /// De platte tekst van het hele document (voor indexering, zoeken, schermlezer).
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

    /// Woordental van het hele document (witruimte-gescheiden tokens).
    pub fn word_count(&self) -> usize {
        self.plain_text().split_whitespace().filter(|w| !w.is_empty()).count()
    }

    /// Tekental (Unicode-tekens) van het hele document.
    pub fn char_count(&self) -> usize {
        self.plain_text().chars().filter(|c| !c.is_whitespace()).count()
    }

    /// Het aantal paragrafen (Writer) / cellen (Sheet) / dia's (Deck).
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
            // Compacte weergave zonder drijvende komma.
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
            b.push(Block::Paragraph(Paragraph::text("Hallo wereld van EuroOS").styled("Heading1")));
            b.push(Block::Paragraph(Paragraph::new().run(Run::new("Vet").bold()).run(Run::new(" en gewoon"))));
        }
        assert_eq!(doc.plain_text(), "Hallo wereld van EuroOS\nVet en gewoon\n");
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
        h1.half_points = Some(40); // 20pt overschrijft
        reg.define("Heading1", Some("Normal"), h1);

        let r = reg.resolve("Heading1");
        assert!(r.bold); // van Heading1
        assert_eq!(r.half_points, Some(40)); // kind overschrijft ouder
        assert_eq!(r.font_family.as_deref(), Some("EuroSans")); // geërfd van Normal
    }

    #[test]
    fn style_cycle_safe() {
        let mut reg = StyleRegistry::new();
        reg.define("A", Some("B"), RunProperties::default());
        reg.define("B", Some("A"), RunProperties::default());
        let _ = reg.resolve("A"); // mag niet hangen
    }

    #[test]
    fn sheet_text() {
        let mut doc = Document::sheet();
        if let Body::Sheet(s) = &mut doc.body {
            s.set(0, 0, Cell::Text(String::from("Omzet")));
            s.set(0, 1, Cell::Number { scaled: 123456, scale: 2 }); // 1234.56
            s.set(1, 0, Cell::Formula(String::from("=SUM(B1:B9)")));
        }
        assert_eq!(doc.plain_text(), "Omzet\t1234.56\t=SUM(B1:B9)");
        assert_eq!(doc.element_count(), 3);
    }

    #[test]
    fn deck_text() {
        let mut doc = Document::deck();
        if let Body::Deck(s) = &mut doc.body {
            s.push(Slide { title: String::from("Welkom"), blocks: alloc::vec![Block::Paragraph(Paragraph::text("Eerste dia"))] });
            s.push(Slide { title: String::from("Einde"), blocks: alloc::vec![] });
        }
        assert_eq!(doc.element_count(), 2);
        assert!(doc.plain_text().contains("Welkom"));
        assert!(doc.plain_text().contains("Eerste dia"));
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
