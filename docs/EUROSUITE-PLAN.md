# EuroSuite — Implementation Plan
**Sovereign Office Suite for EuroOS**  
*Versie 1.0 | Juni 2026 | EUPL-1.2*

---

## 1. Visie & Positionering

EuroSuite is de ingebouwde kantoorapplicatie van EuroOS. Het is geen Linux-port van LibreOffice, maar een **from-scratch sovereign office suite**, geschreven in **Rust** (backend/engine) en **GTK4/Slint** (UI), volledig geïntegreerd met de EuroOS kernel, EuroFS en EuroGuard capability-security model.

### Kernprincipes
- **Compatibiliteit eerst**: native lezen/schrijven van `.docx`, `.xlsx`, `.pptx` (OOXML) én ODF
- **Sovereign by design**: geen telemetrie, geen cloud-vereiste, EU data residency
- **Geïntegreerd met EuroOS**: EuroFS CoW voor undo/versioning, EuroGuard voor document sandboxing
- **Modulair**: drie apps die een gemeenschappelijke engine delen

### Drie applicaties

| App | Equivalent | Formaat |
|---|---|---|
| **EuroSuite Writer** | Microsoft Word | `.docx` / `.odt` |
| **EuroSuite Calc** | Microsoft Excel | `.xlsx` / `.ods` |
| **EuroSuite Impress** | Microsoft PowerPoint | `.pptx` / `.odp` |

---

## 2. Architectuuroverzicht

```
┌─────────────────────────────────────────────────────────┐
│                    EuroSuite                      │
│  ┌───────────┐  ┌───────────┐  ┌───────────────────┐   │
│  │ EuroSuite Writer│  │ EuroSuite Calc │  │    EuroSuite Impress       │   │
│  └─────┬─────┘  └─────┬─────┘  └─────────┬─────────┘   │
│        └──────────────┴──────────────────┘              │
│                        │                                 │
│              ┌─────────▼──────────┐                     │
│              │   EuroSuite Core  │  (gedeelde engine)  │
│              │  - Document Model  │                     │
│              │  - Render Pipeline │                     │
│              │  - Format I/O      │                     │
│              └─────────┬──────────┘                     │
│        ┌───────────────┼───────────────┐                │
│        ▼               ▼               ▼                │
│   ┌─────────┐   ┌─────────────┐  ┌──────────┐          │
│   │ OOXML   │   │    ODF      │  │  PDF/    │          │
│   │ Parser  │   │   Parser    │  │  Export  │          │
│   └─────────┘   └─────────────┘  └──────────┘          │
└─────────────────────────────────────────────────────────┘
         │                    │
    ┌────▼────┐         ┌─────▼─────┐
    │ EuroFS  │         │ EuroGuard │
    │ (CoW)   │         │ (sandbox) │
    └─────────┘         └───────────┘
```

---

## 3. Tech Stack

### Primaire talen & frameworks

```toml
# EuroSuite workspace - Cargo.toml
[workspace]
members = [
    "crates/euro-suite-core",   # Gedeelde document engine
    "crates/euro-suite-writer",        # Tekstverwerker
    "crates/euro-suite-calc",         # Spreadsheet
    "crates/euro-suite-impress",          # Presentaties
    "crates/ooxml-parser",       # OOXML (docx/xlsx/pptx) I/O
    "crates/odf-parser",         # ODF I/O
    "crates/pdf-export",         # PDF generatie
]
```

### UI Framework keuze: **Slint**

Slint is een Rust-native declaratieve UI toolkit, ideaal voor EuroOS:
- Compileert naar native code (geen Electron, geen WebView)
- Werkt zonder X11 of Wayland daemon (eigen rendering via EuroDesktop)
- Kleine binary footprint (~5MB per app)
- Kan later GPU-versneld worden via EuroNet/Vulkan

**Alternatief**: GTK4 via `gtk4-rs` als bredere ecosysteemcompatibiliteit gewenst is.

### Kritische Rust crates

| Crate | Doel |
|---|---|
| `quick-xml` | XML parsing (OOXML/ODF basis) |
| `zip` | ZIP archief I/O (docx = ZIP + XML) |
| `unicode-segmentation` | Correcte tekst cursor beweging |
| `unicode-bidi` | RTL/LTR tekst support |
| `rustybuzz` | Text shaping (HarfBuzz port in Rust) |
| `fontdue` | Font rasterization |
| `image` | Afbeeldingen in documenten |
| `lopdf` | PDF generatie |
| `calamine` | Excel lezen (bootstrap, later vervangen) |
| `chrono` | Datum/tijd in spreadsheets |
| `regex` | Zoeken & vervangen |
| `serde` + `serde_json` | Interne serialisatie |
| `rayon` | Parallelle rendering (grote sheets) |

---

## 4. EuroSuite Core — Document Model

De kern van EuroSuite is een **taal-agnostisch document model** dat alle drie apps delen.

### 4.1 Universeel Document Model (UDM)

```rust
// crates/euro-suite-core/src/model.rs

/// Top-level document container
pub struct Document {
    pub kind: DocumentKind,
    pub metadata: DocumentMetadata,
    pub body: DocumentBody,
    pub styles: StyleRegistry,
    pub resources: ResourceStore,   // afbeeldingen, fonts, embeds
    pub revision_history: Vec<Revision>,
}

pub enum DocumentKind {
    Writer,   // tekstdocument
    Sheet,    // spreadsheet
    Deck,     // presentatie
}

pub struct DocumentMetadata {
    pub title: String,
    pub author: String,
    pub created: chrono::DateTime<chrono::Utc>,
    pub modified: chrono::DateTime<chrono::Utc>,
    pub language: LanguageTag,      // nl-BE, fr-BE, en-GB, ...
}

/// Writer body
pub enum DocumentBody {
    Writer(WriterBody),
    Sheet(SheetBody),
    Deck(DeckBody),
}

pub struct WriterBody {
    pub sections: Vec<Section>,
}

pub struct Section {
    pub properties: SectionProperties,
    pub blocks: Vec<Block>,
}

/// Blokken = paragrafen, tabellen, afbeeldingen
pub enum Block {
    Paragraph(Paragraph),
    Table(Table),
    Image(InlineImage),
    PageBreak,
    SectionBreak,
    HorizontalRule,
}
```

### 4.2 Tekst & Formattering (Writer)

```rust
pub struct Paragraph {
    pub style_id: Option<StyleId>,
    pub properties: ParagraphProperties,
    pub runs: Vec<Run>,
}

pub struct ParagraphProperties {
    pub alignment: Alignment,
    pub indent: Indent,
    pub spacing: Spacing,
    pub list: Option<ListProperties>,
    pub border: Option<Border>,
    pub shading: Option<Shading>,
}

pub struct Run {
    pub text: String,
    pub properties: RunProperties,
}

pub struct RunProperties {
    pub font_family: Option<String>,
    pub font_size: Option<f32>,         // in points
    pub bold: bool,
    pub italic: bool,
    pub underline: Option<UnderlineStyle>,
    pub strikethrough: bool,
    pub color: Option<Color>,
    pub highlight: Option<Color>,
    pub language: Option<LanguageTag>,
    pub hyperlink: Option<Url>,
}

pub enum Alignment {
    Left, Center, Right, Justify,
}
```

### 4.3 Spreadsheet Model (Sheet)

```rust
pub struct SheetBody {
    pub sheets: Vec<Worksheet>,
    pub named_ranges: Vec<NamedRange>,
    pub shared_strings: Vec<String>,
}

pub struct Worksheet {
    pub name: String,
    pub cells: HashMap<CellAddress, Cell>,
    pub columns: Vec<ColumnProperties>,
    pub rows: Vec<RowProperties>,
    pub merged_cells: Vec<CellRange>,
    pub charts: Vec<Chart>,
    pub conditional_formats: Vec<ConditionalFormat>,
}

pub struct Cell {
    pub value: CellValue,
    pub formula: Option<Formula>,
    pub style: CellStyle,
    pub comment: Option<String>,
}

pub enum CellValue {
    Empty,
    Text(String),
    Number(f64),
    Boolean(bool),
    Error(FormulaError),
    Date(chrono::NaiveDate),
    DateTime(chrono::NaiveDateTime),
}

pub struct Formula {
    pub expression: String,     // "=SUM(A1:A10)"
    pub cached_value: CellValue,
}
```

### 4.4 Presentatie Model (Deck)

```rust
pub struct DeckBody {
    pub slides: Vec<Slide>,
    pub slide_masters: Vec<SlideMaster>,
    pub slide_layouts: Vec<SlideLayout>,
    pub theme: Theme,
}

pub struct Slide {
    pub id: SlideId,
    pub layout: SlideLayoutRef,
    pub shapes: Vec<Shape>,
    pub notes: Option<NotesSlide>,
    pub transition: Option<Transition>,
    pub animations: Vec<Animation>,
    pub background: Background,
}

pub enum Shape {
    TextBox(TextBox),
    Image(ImageShape),
    Chart(Chart),
    Table(SlideTable),
    SmartArt(SmartArt),
    Connector(Connector),
    GroupShape(Vec<Shape>),
}
```

---

## 5. OOXML Parser — Compatibiliteit met Word/Excel/PowerPoint

OOXML (Office Open XML) is het formaat van `.docx`, `.xlsx` en `.pptx`. Het is een ZIP-archief met XML-bestanden.

### 5.1 Structuur van een .docx bestand

```
document.docx (ZIP)
├── [Content_Types].xml
├── _rels/.rels
└── word/
    ├── document.xml        ← hoofdinhoud
    ├── styles.xml          ← stijldefinities
    ├── numbering.xml       ← lijsten
    ├── settings.xml
    ├── fontTable.xml
    ├── _rels/
    │   └── document.xml.rels
    └── media/
        └── image1.png
```

### 5.2 Parser implementatie

```rust
// crates/ooxml-parser/src/docx.rs

use quick_xml::Reader;
use zip::ZipArchive;

pub struct DocxParser;

impl DocxParser {
    pub fn parse(bytes: &[u8]) -> Result<Document, OoxmlError> {
        let cursor = std::io::Cursor::new(bytes);
        let mut archive = ZipArchive::new(cursor)?;
        
        // 1. Lees content types
        let content_types = Self::parse_content_types(&mut archive)?;
        
        // 2. Parse relationships
        let relationships = Self::parse_relationships(&mut archive)?;
        
        // 3. Parse hoofddocument
        let body = Self::parse_document_xml(&mut archive)?;
        
        // 4. Parse stijlen
        let styles = Self::parse_styles(&mut archive)?;
        
        // 5. Parse nummering (lijsten)
        let numbering = Self::parse_numbering(&mut archive)?;
        
        // 6. Extraheer media (afbeeldingen)
        let resources = Self::extract_media(&mut archive)?;
        
        Ok(Document {
            kind: DocumentKind::Writer,
            body: DocumentBody::Writer(body),
            styles,
            resources,
            ..Default::default()
        })
    }
    
    fn parse_document_xml(archive: &mut ZipArchive<impl Read + Seek>) 
        -> Result<WriterBody, OoxmlError> 
    {
        let mut file = archive.by_name("word/document.xml")?;
        let mut xml = String::new();
        file.read_to_string(&mut xml)?;
        
        let mut reader = Reader::from_str(&xml);
        let mut blocks = Vec::new();
        let mut buf = Vec::new();
        
        loop {
            match reader.read_event_into(&mut buf)? {
                Event::Start(ref e) if e.name().as_ref() == b"w:p" => {
                    blocks.push(Block::Paragraph(
                        Self::parse_paragraph(&mut reader)?
                    ));
                }
                Event::Start(ref e) if e.name().as_ref() == b"w:tbl" => {
                    blocks.push(Block::Table(
                        Self::parse_table(&mut reader)?
                    ));
                }
                Event::Eof => break,
                _ => {}
            }
            buf.clear();
        }
        
        Ok(WriterBody {
            sections: vec![Section {
                properties: SectionProperties::default(),
                blocks,
            }]
        })
    }
}
```

### 5.3 OOXML Schrijver (serialisatie)

```rust
// crates/ooxml-parser/src/docx_writer.rs

pub struct DocxWriter;

impl DocxWriter {
    pub fn serialize(doc: &Document) -> Result<Vec<u8>, OoxmlError> {
        let mut zip_buf = Vec::new();
        let cursor = std::io::Cursor::new(&mut zip_buf);
        let mut zip = ZipWriter::new(cursor);
        
        // Content types
        zip.start_file("[Content_Types].xml", Default::default())?;
        zip.write_all(Self::content_types_xml().as_bytes())?;
        
        // Relationships
        zip.start_file("_rels/.rels", Default::default())?;
        zip.write_all(Self::root_rels_xml().as_bytes())?;
        
        // Hoofddocument
        zip.start_file("word/document.xml", Default::default())?;
        let body_xml = Self::serialize_body(doc)?;
        zip.write_all(body_xml.as_bytes())?;
        
        // Stijlen
        zip.start_file("word/styles.xml", Default::default())?;
        zip.write_all(Self::serialize_styles(&doc.styles)?.as_bytes())?;
        
        // Media
        for (name, data) in &doc.resources.images {
            zip.start_file(format!("word/media/{}", name), Default::default())?;
            zip.write_all(data)?;
        }
        
        zip.finish()?;
        Ok(zip_buf)
    }
}
```

### 5.4 Formule Engine voor EuroSuite Calc

Spreadsheet formules zijn de meest complexe feature. Implementatie in fasen:

```rust
// crates/euro-suite-core/src/formula/mod.rs

pub struct FormulaEngine {
    functions: HashMap<String, Box<dyn SpreadsheetFunction>>,
}

impl FormulaEngine {
    pub fn new() -> Self {
        let mut engine = Self { functions: HashMap::new() };
        
        // Fase 1: Basis (MVP)
        engine.register(SumFunction);
        engine.register(AverageFunction);
        engine.register(CountFunction);
        engine.register(MinFunction);
        engine.register(MaxFunction);
        engine.register(IfFunction);
        engine.register(AndFunction);
        engine.register(OrFunction);
        engine.register(NotFunction);
        engine.register(ConcatFunction);
        engine.register(LenFunction);
        engine.register(UpperFunction);
        engine.register(LowerFunction);
        
        // Fase 2: Geavanceerd
        engine.register(VlookupFunction);
        engine.register(HlookupFunction);
        engine.register(IndexMatchFunction);
        engine.register(SumIfFunction);
        engine.register(CountIfFunction);
        engine.register(DateFunction);
        engine.register(TodayFunction);
        engine.register(RoundFunction);
        
        // Fase 3: Power user
        engine.register(XlookupFunction);
        engine.register(ArrayFormulaSupport);
        engine.register(PivotEngine);
        
        engine
    }
    
    pub fn evaluate(&self, formula: &str, context: &SheetContext) 
        -> Result<CellValue, FormulaError> 
    {
        let ast = self.parse(formula)?;
        self.eval_node(&ast, context)
    }
}
```

---

## 6. Rendering Pipeline

De render pipeline zet het document model om naar pixels op scherm.

### 6.1 Writer Renderer

```
Document Model
      │
      ▼
Layout Engine
  - Paginabreedte/hoogte berekenen
  - Tekstflow: tekst wrappen over regels
  - Tabel layout: kolombreedte berekenen
  - Afbeeldingen positioneren
      │
      ▼
Render Tree
  - Elke node = een visueel element met positie + afmetingen
      │
      ▼
Rasterizer (Fontdue + custom)
  - Tekst → glyphs via rustybuzz (shaping)
  - Glyphs → pixels via fontdue
  - Afbeeldingen → gedecodeerd via `image` crate
      │
      ▼
Display Buffer → EuroDesktop compositor
```

### 6.2 Text Shaping (correct Unicode)

```rust
// Kritisch voor Belgische talen (nl/fr/de) en speciale tekens

use rustybuzz::{Face, UnicodeBuffer};

pub fn shape_text(text: &str, font_data: &[u8], font_size: f32) -> Vec<GlyphInfo> {
    let face = Face::from_slice(font_data, 0).unwrap();
    let mut buffer = UnicodeBuffer::new();
    buffer.push_str(text);
    
    let output = rustybuzz::shape(&face, &[], buffer);
    
    output.glyph_infos()
        .iter()
        .zip(output.glyph_positions())
        .map(|(info, pos)| GlyphInfo {
            glyph_id: info.glyph_id,
            x_advance: pos.x_advance as f32 / 64.0 * font_size / face.units_per_em() as f32,
            y_advance: pos.y_advance as f32 / 64.0 * font_size / face.units_per_em() as f32,
            x_offset: pos.x_offset as f32,
            y_offset: pos.y_offset as f32,
        })
        .collect()
}
```

---

## 7. EuroSuite Writer UI — Slint Component Structuur

```
EuroSuite WriterWindow
├── MenuBar
│   ├── FileMenu (Nieuw, Openen, Opslaan, Exporteer naar PDF...)
│   ├── EditMenu (Ongedaan maken, Knippen, Kopiëren, Plakken, Zoeken...)
│   ├── InsertMenu (Afbeelding, Tabel, Link, Paginanummer...)
│   ├── FormaatMenu (Lettertype, Alinea, Stijlen, Kolommen...)
│   └── HulpMenu
├── ToolbarRibbon
│   ├── FontSelector (dropdown)
│   ├── FontSizeSelector
│   ├── FormattingButtons (Vet, Cursief, Onderstrepen, ...)
│   ├── AlignmentButtons (Links, Gecentreerd, Rechts, Uitvullen)
│   └── StyleSelector
├── DocumentCanvas
│   ├── PageView (één of meerdere pagina's)
│   │   └── EditableTextLayer (cursor, selectie, tekst invoer)
│   └── ScrollBar
├── StatusBar
│   ├── WordCount
│   ├── PageNumber
│   ├── ZoomControl
│   └── LanguageIndicator
└── SidePanel (optioneel)
    ├── StylesPanel
    ├── NavigatorPanel
    └── CommentsPanel
```

### 7.1 Cursor & Selectie Engine

```rust
pub struct EditorState {
    pub document: Document,
    pub cursor: DocumentCursor,
    pub selection: Option<DocumentRange>,
    pub viewport: Viewport,
    pub undo_stack: Vec<EditOperation>,
    pub redo_stack: Vec<EditOperation>,
}

pub struct DocumentCursor {
    pub block_index: usize,
    pub run_index: usize,
    pub char_offset: usize,   // byte offset in UTF-8
}

impl EditorState {
    pub fn insert_text(&mut self, text: &str) {
        let op = EditOperation::Insert {
            position: self.cursor.clone(),
            text: text.to_string(),
        };
        self.apply_operation(&op);
        self.undo_stack.push(op);
        self.redo_stack.clear();
    }
    
    pub fn undo(&mut self) {
        if let Some(op) = self.undo_stack.pop() {
            let inverse = op.inverse();
            self.apply_operation(&inverse);
            self.redo_stack.push(op);
        }
    }
}
```

---

## 8. Integratie met EuroOS

### 8.1 EuroFS CoW voor Undo/Versioning

EuroOS zijn CoW (Copy-on-Write) filesystem laat toe om document snapshots extreem efficiënt op te slaan:

```rust
// Bij elke Save → EuroFS snapshot
pub fn save_with_snapshot(doc: &Document, path: &EuroPath) -> Result<()> {
    let bytes = DocxWriter::serialize(doc)?;
    
    // EuroFS CoW snapshot = bijna gratis (alleen gewijzigde blokken)
    euro_fs::write_with_snapshot(path, &bytes)?;
    
    // Versiegeschiedenis automatisch beschikbaar via EuroFS
    Ok(())
}

// Versiegeschiedenis tonen
pub fn list_versions(path: &EuroPath) -> Vec<FileVersion> {
    euro_fs::list_snapshots(path)
        .into_iter()
        .map(|snap| FileVersion {
            timestamp: snap.created,
            size: snap.size,
            restore_fn: move || euro_fs::restore_snapshot(&snap),
        })
        .collect()
}
```

### 8.2 EuroGuard Document Sandboxing

Documenten met macro's of embedded scripts worden in een EuroGuard capability sandbox uitgevoerd:

```rust
// EuroGuard capability token voor document
pub struct DocumentCapabilities {
    pub can_read_filesystem: bool,      // false voor externe documenten
    pub can_write_filesystem: bool,     // false standaard
    pub can_network_access: bool,       // false standaard
    pub can_execute_scripts: bool,      // false, tenzij gebruiker goedkeurt
    pub allowed_paths: Vec<EuroPath>,   // whitelist van toegestane paden
}

impl Default for DocumentCapabilities {
    fn default() -> Self {
        // Minimale rechten by default
        Self {
            can_read_filesystem: false,
            can_write_filesystem: false,
            can_network_access: false,
            can_execute_scripts: false,
            allowed_paths: vec![],
        }
    }
}
```

### 8.3 EuroNet Collaboratie (Toekomst: Sprint H+)

Real-time samenwerking via EuroNet:

```rust
// CRDT (Conflict-free Replicated Data Type) voor gelijktijdig bewerken
pub struct CollaborativeDocument {
    pub doc: Document,
    pub crdt: automerge::Automerge,     // of eigen CRDT implementatie
    pub peers: Vec<PeerConnection>,
    pub awareness: AwarenessState,      // cursors van andere gebruikers
}
```

---

## 9. PDF Export

PDF export is essentieel voor professionele documenten:

```rust
// crates/pdf-export/src/lib.rs

use lopdf::{Document as PdfDoc, Object, Stream};

pub struct PdfExporter;

impl PdfExporter {
    pub fn export(doc: &Document) -> Result<Vec<u8>, ExportError> {
        let layout = LayoutEngine::layout(doc)?;
        let mut pdf = PdfDoc::new();
        
        for page in &layout.pages {
            let pdf_page = pdf.new_object(
                dictionary! {
                    "Type" => "Page",
                    "MediaBox" => vec![0, 0, page.width_pt, page.height_pt],
                }
            );
            
            // Render tekst naar PDF text streams
            for text_block in &page.text_blocks {
                Self::render_text_to_pdf(&mut pdf, pdf_page, text_block)?;
            }
            
            // Render afbeeldingen
            for image in &page.images {
                Self::render_image_to_pdf(&mut pdf, pdf_page, image)?;
            }
        }
        
        let mut bytes = Vec::new();
        pdf.save_to(&mut bytes)?;
        Ok(bytes)
    }
}
```

---

## 10. Implementatie Roadmap

### Sprint A — Fundament (6 weken)
- [ ] `euro-suite-core` crate met basis document model
- [ ] `ooxml-parser`: `.docx` lezen (tekst, basis formattering, stijlen)
- [ ] `ooxml-parser`: `.docx` schrijven (round-trip test)
- [ ] Basis tekst layout engine (geen paginering, geen afbeeldingen)
- [ ] Minimale Slint UI: tekst weergeven en bewerken
- [ ] Cursor beweging: pijltjestoetsen, Home/End, Ctrl+pijl
- [ ] Basis toetsenbord invoer

**Milestone**: Een `.docx` openen, tekst bewerken, opslaan, opnieuw openen in Word → identiek

### Sprint B — Writer MVP (6 weken)
- [ ] Paginering en paginaweergave
- [ ] Alinea formattering (uitlijning, inspringing, regelafstand)
- [ ] Karakter formattering (vet, cursief, onderstrepen, kleur, lettertype)
- [ ] Kopiëren/plakken (intern + klembord interop met EuroOS)
- [ ] Zoeken & vervangen
- [ ] Undo/redo (20 niveaus minimum)
- [ ] Stijlen paneel (Normaal, Kop 1-6, Citaat, ...)
- [ ] Afbeeldingen invoegen

**Milestone**: EuroSuite Writer bruikbaar voor dagelijkse tekstverwerking

### Sprint C — Sheet MVP (8 weken)
- [ ] `ooxml-parser`: `.xlsx` lezen en schrijven
- [ ] Grid weergave: cellen, kolommen, rijen
- [ ] Cel bewerking: tekst, getallen, datums
- [ ] Formule engine: 30 basisfuncties (SUM, AVERAGE, IF, VLOOKUP, ...)
- [ ] Cel formattering: randen, achtergrondkleur, getalformaten
- [ ] Kolom/rij formaat aanpassen (slepen)
- [ ] Meerdere bladen (tabbladen)
- [ ] Cellen samenvoegen/splitsen

**Milestone**: EuroSuite Calc bruikbaar voor basisspreadsheets en budgetbeheer

### Sprint D — Deck MVP (6 weken)
- [ ] `ooxml-parser`: `.pptx` lezen en schrijven
- [ ] Diasweergave met miniaturen
- [ ] Tekstvakken op dia's bewerken
- [ ] Achtergronden, thema's, kleurenschema's
- [ ] Afbeeldingen en vormen
- [ ] Presentatieweergave (fullscreen)
- [ ] Sprekernotities

**Milestone**: EuroSuite Impress bruikbaar voor basisdiapresentaties

### Sprint E — Kwaliteit & Compatibiliteit (4 weken)
- [ ] ODF support (`.odt`, `.ods`, `.odp`) lezen/schrijven
- [ ] PDF export voor alle drie apps
- [ ] Compatibiliteitstests: 100 echte Word/Excel/PowerPoint documenten
- [ ] Spellingscontrole (nl-BE, fr-BE, de-BE, en-GB) via Hunspell
- [ ] Automatisch opslaan (elke 30 seconden)
- [ ] Recent geopende bestanden

### Sprint F — EuroOS Integratie (3 weken)
- [ ] EuroFS CoW versiegeschiedenis in UI
- [ ] EuroGuard document sandboxing
- [ ] EuroOS bestandsdialogen integreren
- [ ] MIME type registratie bij EuroDesktop
- [ ] Standaard apps instelling

### Sprint G — Geavanceerde Features (ongoing)
- [ ] Grafieken in EuroSuite Calc (staaf, lijn, cirkel, ...)
- [ ] Tabellen in EuroSuite Writer (cellen samenvoegen, tabelstijlen)
- [ ] Inhoudsopgave automatisch genereren
- [ ] Commentaren en bijhouden wijzigingen
- [ ] Voetteksten en kopteksten
- [ ] Meerdere kolommen in Writer
- [ ] Conditionele formattering in Sheet
- [ ] Animaties in Deck
- [ ] Real-time collaboratie via EuroNet (CRDT)

---

## 11. Compatibiliteitsstrategie

### Testmatrix

Elke sprint draaien we automatische compatibiliteitstests:

```bash
# Test suite: round-trip compatibiliteit
# Werkwijze: docx lezen → document model → docx schrijven → 
#            vergelijken met Microsoft Word output

cargo test --test compat_docx    # 50 test documenten
cargo test --test compat_xlsx    # 30 test spreadsheets
cargo test --test compat_pptx    # 20 test presentaties
```

### Bekende OOXML complexiteiten

| Feature | Prioriteit | Aanpak |
|---|---|---|
| Lettertype fallback | Hoog | Bundel Liberation fonts (metrisch compatibel met Arial/Times) |
| Track Changes | Medium | Tonen maar niet bewerken in Sprint A-D |
| Macro's (VBA) | Laag | Nooit uitvoeren, wel bewaren in ZIP |
| DDE / OLE embeds | Laag | Weergeven als placeholder |
| Legacy `.doc` / `.xls` | Laag | Conversiehint → "Sla op als .docx" |
| Smart Art | Medium | Converteren naar statische afbeelding |

### Fonts — Liberation Font Set

Microsoft fonts (Arial, Times New Roman, Calibri) bundelen we NIET. In plaats daarvan:

```
Liberation Sans     ↔  Arial (metrisch identiek)
Liberation Serif    ↔  Times New Roman (metrisch identiek)
Liberation Mono     ↔  Courier New (metrisch identiek)
Caladea             ↔  Cambria (metrisch identiek)
Carlito              ↔  Calibri (metrisch identiek)
```

Dit garandeert dat pagina-indeling identiek blijft aan documenten gemaakt in Word.

---

## 12. Repository Structuur

```
euro-suite/
├── Cargo.toml                          # workspace
├── Cargo.lock
├── LICENSE                             # EUPL-1.2
├── README.md
├── CONTRIBUTING.md
├── crates/
│   ├── euro-suite-core/
│   │   ├── src/
│   │   │   ├── model/                  # Document model types
│   │   │   ├── formula/                # Formule engine
│   │   │   ├── layout/                 # Layout engine
│   │   │   ├── render/                 # Render pipeline
│   │   │   └── lib.rs
│   │   └── Cargo.toml
│   ├── ooxml-parser/
│   │   ├── src/
│   │   │   ├── docx/                   # .docx parser + writer
│   │   │   ├── xlsx/                   # .xlsx parser + writer
│   │   │   ├── pptx/                   # .pptx parser + writer
│   │   │   └── lib.rs
│   │   └── Cargo.toml
│   ├── odf-parser/
│   │   └── src/                        # .odt/.ods/.odp
│   ├── pdf-export/
│   │   └── src/
│   ├── euro-suite-writer/
│   │   ├── src/
│   │   │   ├── ui/                     # Slint UI components
│   │   │   ├── editor/                 # Editor state machine
│   │   │   └── main.rs
│   │   └── Cargo.toml
│   ├── euro-suite-calc/
│   │   └── src/
│   └── euro-suite-impress/
│       └── src/
├── fonts/                              # Gebundelde Liberation fonts
├── tests/
│   ├── compat/                         # Compatibiliteitstestdocumenten
│   └── integration/
└── docs/
    ├── architecture.md
    ├── ooxml-notes.md                  # OOXML edge cases documentatie
    └── formula-reference.md
```

---

## 13. Geschatte Inspanning

| Sprint | Duur | Ontwikkelaars | Focusgebied |
|---|---|---|---|
| A — Fundament | 6 weken | 2 | Core, OOXML lezen/schrijven |
| B — Writer MVP | 6 weken | 2 | UI, layout, formattering |
| C — Sheet MVP | 8 weken | 2-3 | Grid, formules |
| D — Deck MVP | 6 weken | 2 | Slides, thema's |
| E — Kwaliteit | 4 weken | 2 | ODF, PDF, compat tests |
| F — EuroOS integratie | 3 weken | 1-2 | EuroFS, EuroGuard |
| **Totaal MVP** | **~33 weken** | | |

Voor een **solosoftwareontwikkelaar** met Claude Code als primaire partner: schat 12-18 maanden voor een stabiele v1.0.

---

## 14. Externe Referentie-implementaties (voor inspiratie)

Bekijk de source code van:
- **Calligra Suite** (KDE) — C++, maar goede OOXML aanpak
- **ONLYOFFICE** — open source, goede Word compatibiliteit
- **Collabora Online** (LibreOffice web) — hoe ze OOXML afhandelen
- **docx-rs** (Rust crate) — partiële .docx implementatie in Rust
- **umya-spreadsheet** (Rust crate) — .xlsx lezen/schrijven in Rust

> **Belangrijk**: Gebruik deze als referentie voor OOXML edge cases, niet als basis voor EuroSuite. EuroSuite wordt from-scratch gebouwd onder EUPL-1.2.

---

## 15. Licentie & Soevereiniteit

```
Copyright (C) 2026 GoTrust BV / EuroOS Project
Licentie: EUPL-1.2 (European Union Public Licence)

EuroSuite wordt ontwikkeld in België, gehost in de EU,
zonder afhankelijkheid van Amerikaanse tech-bedrijven.

Gebundelde fonts: SIL Open Font License 1.1
```

---

*EuroSuite — Europese soevereiniteit begint bij het document.*
