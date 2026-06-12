//! Het Universeel Document Model (UDM) — één boom voor tekst, rekenblad én
//! presentatie. OOXML/ODF/PDF worden hierín geparsed (ES-IO) en de apps
//! (Writer/Calc/Impress) renderen + bewerken hém. Eén model, drie views.

use alloc::string::String;
use alloc::vec::Vec;

/// Het soort document.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DocumentKind {
    Writer, // tekstdocument
    Sheet,  // rekenblad
    Deck,   // presentatie
}

/// Een RGB-kleur.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// Paragraaf-uitlijning.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Alignment {
    #[default]
    Left,
    Center,
    Right,
    Justify,
}

/// Teken-opmaak van een tekst-run.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct RunProperties {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    /// Lettergrootte in halve punten (zo blijft 10,5pt geheeltallig); `None` = erven.
    pub half_points: Option<u16>,
    pub color: Option<Color>,
    pub font_family: Option<String>,
}

impl RunProperties {
    /// Overschrijf alleen de gezette velden van `over` over `self` (overerving).
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

/// Een aaneengesloten stuk tekst met uniforme opmaak.
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

/// Paragraaf-opmaak.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct ParagraphProperties {
    pub alignment: Alignment,
    /// Naar welke benoemde stijl verwijst deze paragraaf (bv. "Heading1")?
    pub style_id: Option<String>,
    /// Lijstniveau (0-gebaseerd) als de paragraaf een lijstitem is.
    pub list_level: Option<u8>,
}

/// Een paragraaf = opmaak + een rij runs.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Paragraph {
    pub props: ParagraphProperties,
    pub runs: Vec<Run>,
}

impl Paragraph {
    pub fn new() -> Paragraph {
        Paragraph::default()
    }
    /// Een paragraaf met één platte-tekst-run.
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
    /// De platte tekst van de paragraaf (alle runs aaneen).
    pub fn plain_text(&self) -> String {
        let mut s = String::new();
        for r in &self.runs {
            s.push_str(&r.text);
        }
        s
    }
}

/// Een tabel: rijen × cellen, elke cel een rij paragrafen.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Table {
    pub rows: Vec<Vec<Vec<Paragraph>>>,
}

/// Een blok in een tekstdocument.
#[derive(Clone, PartialEq, Debug)]
pub enum Block {
    Paragraph(Paragraph),
    Table(Table),
    Image { alt: String, width: u32, height: u32 },
    PageBreak,
    HorizontalRule,
}

// ── Rekenblad ───────────────────────────────────────────────────────────────

/// De inhoud van een cel.
#[derive(Clone, PartialEq, Debug)]
pub enum Cell {
    Empty,
    /// Een getal als geschaalde integer (waarde × 10^`scale`) — geen drijvende komma.
    Number { scaled: i64, scale: u8 },
    Text(String),
    /// Een formule (de brontekst, bv. `"=A1+B2"`); de Calc-engine evalueert 'm.
    Formula(String),
}

/// Een rekenblad: een dunne (sparse) cellijst geïndexeerd op (rij, kolom), 0-gebaseerd.
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

// ── Presentatie ─────────────────────────────────────────────────────────────

/// Een dia: een titel + blokken (zoals een tekstdocument-sectie).
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Slide {
    pub title: String,
    pub blocks: Vec<Block>,
}

/// De body verschilt per documentsoort.
#[derive(Clone, PartialEq, Debug)]
pub enum Body {
    Writer(Vec<Block>),
    Sheet(SheetBody),
    Deck(Vec<Slide>),
}

/// Documentmetadata.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Metadata {
    pub title: String,
    pub author: String,
    /// BCP-47-taal-tag (nl-BE, fr-BE, …) — koppelt aan EuroLocale.
    pub language: String,
}
