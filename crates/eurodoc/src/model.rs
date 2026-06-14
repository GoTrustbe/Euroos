//! The Universal Document Model (UDM) — one tree for text, spreadsheet, and
//! presentation. OOXML/ODF/PDF are parsed into this (ES-IO) and the apps
//! (Writer/Calc/Impress) render + edit it. One model, three views.

use alloc::string::String;
use alloc::vec::Vec;

/// The kind of document.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DocumentKind {
    Writer, // text document
    Sheet,  // spreadsheet
    Deck,   // presentation
}

/// An RGB color.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// Paragraph alignment.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Alignment {
    #[default]
    Left,
    Center,
    Right,
    Justify,
}

/// Character formatting of a text run.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct RunProperties {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    /// Font size in half-points (so 10.5pt stays integral); `None` = inherit.
    pub half_points: Option<u16>,
    pub color: Option<Color>,
    pub font_family: Option<String>,
}

impl RunProperties {
    /// Override only the set fields of `over` onto `self` (inheritance).
    pub fn merge(&self, over: &RunProperties) -> RunProperties {
        RunProperties {
            bold: self.bold || over.bold,
            italic: self.italic || over.italic,
            underline: self.underline || over.underline,
            strikethrough: self.strikethrough || over.strikethrough,
            half_points: over.half_points.or(self.half_points),
            color: over.color.or(self.color),
            font_family: over.font_family.clone().or_else(|| self.font_family.clone()),
        }
    }
}

/// A contiguous piece of text with uniform formatting.
#[derive(Clone, PartialEq, Debug)]
pub struct Run {
    pub text: String,
    pub props: RunProperties,
}

impl Run {
    pub fn new(text: &str) -> Run {
        Run { text: String::from(text), props: RunProperties::default() }
    }
    pub fn bold(mut self) -> Run {
        self.props.bold = true;
        self
    }
    pub fn italic(mut self) -> Run {
        self.props.italic = true;
        self
    }
}

/// Paragraph formatting.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct ParagraphProperties {
    pub alignment: Alignment,
    /// Which named style does this paragraph reference (e.g. "Heading1")?
    pub style_id: Option<String>,
    /// List level (0-based) if the paragraph is a list item.
    pub list_level: Option<u8>,
}

/// A paragraph = formatting + a series of runs.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Paragraph {
    pub props: ParagraphProperties,
    pub runs: Vec<Run>,
}

impl Paragraph {
    pub fn new() -> Paragraph {
        Paragraph::default()
    }
    /// A paragraph with a single plain-text run.
    pub fn text(s: &str) -> Paragraph {
        Paragraph { props: ParagraphProperties::default(), runs: alloc::vec![Run::new(s)] }
    }
    pub fn styled(mut self, style: &str) -> Paragraph {
        self.props.style_id = Some(String::from(style));
        self
    }
    pub fn run(mut self, r: Run) -> Paragraph {
        self.runs.push(r);
        self
    }
    /// The plain text of the paragraph (all runs concatenated).
    pub fn plain_text(&self) -> String {
        let mut s = String::new();
        for r in &self.runs {
            s.push_str(&r.text);
        }
        s
    }
}

/// A table: rows × cells, each cell a series of paragraphs.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Table {
    pub rows: Vec<Vec<Vec<Paragraph>>>,
}

/// A block in a text document.
#[derive(Clone, PartialEq, Debug)]
pub enum Block {
    Paragraph(Paragraph),
    Table(Table),
    Image { alt: String, width: u32, height: u32 },
    PageBreak,
    HorizontalRule,
}

// ── Spreadsheet ───────────────────────────────────────────────────────────────

/// The content of a cell.
#[derive(Clone, PartialEq, Debug)]
pub enum Cell {
    Empty,
    /// A number as a scaled integer (value × 10^`scale`) — no floating point.
    Number { scaled: i64, scale: u8 },
    Text(String),
    /// A formula (the source text, e.g. `"=A1+B2"`); the Calc engine evaluates it.
    Formula(String),
}

/// A spreadsheet: a sparse cell list indexed by (row, column), 0-based.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct SheetBody {
    pub cells: Vec<(u32, u32, Cell)>,
}

impl SheetBody {
    pub fn set(&mut self, row: u32, col: u32, cell: Cell) {
        if let Some(e) = self.cells.iter_mut().find(|(r, c, _)| *r == row && *c == col) {
            e.2 = cell;
        } else {
            self.cells.push((row, col, cell));
        }
    }
    pub fn get(&self, row: u32, col: u32) -> Cell {
        self.cells.iter().find(|(r, c, _)| *r == row && *c == col).map(|(_, _, v)| v.clone()).unwrap_or(Cell::Empty)
    }
}

// ── Presentation ─────────────────────────────────────────────────────────────

/// A slide: a title + blocks (like a text-document section).
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Slide {
    pub title: String,
    pub blocks: Vec<Block>,
}

/// The body differs per document kind.
#[derive(Clone, PartialEq, Debug)]
pub enum Body {
    Writer(Vec<Block>),
    Sheet(SheetBody),
    Deck(Vec<Slide>),
}

/// Document metadata.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Metadata {
    pub title: String,
    pub author: String,
    /// BCP-47 language tag (nl-BE, fr-BE, …) — links to EuroLocale.
    pub language: String,
}
