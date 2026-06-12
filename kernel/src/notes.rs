//! Kernel-zijde van **EuroNotes** (Sprint AC-1): de notitie-app.
//! Bij boot bewijzen we de Markdown→EuroDoc-pijplijn: koppen, inline-opmaak,
//! lijsten met niveaus, en `#tag`-extractie. Host-geteste kern: [`euronotes`].

use crate::serial_println;
use eurodoc::model::Block;

/// Boot-zelftest: parse een notitie en controleer titel, blokken en tags.
pub fn selftest() {
    let md = "# Sprintplan #euros\n\n\
              Doelen voor #q3-2026:\n\n\
              - EuroWeb engine\n\
              - EuroReken\n  - bitwise modus\n\n\
              Status is **goed** en *stabiel*.\n\n\
              > Soevereiniteit door ontwerp.\n";
    let note = euronotes::parse(md);

    let headings = note
        .blocks
        .iter()
        .filter(|b| matches!(b, Block::Paragraph(p) if p.props.style_id.as_deref() == Some("Heading1")))
        .count();
    let list_items = note
        .blocks
        .iter()
        .filter(|b| matches!(b, Block::Paragraph(p) if p.props.list_level.is_some()))
        .count();
    let nested = note.blocks.iter().any(|b| {
        matches!(b, Block::Paragraph(p) if p.props.list_level == Some(1))
    });
    let has_tags = note.tags.iter().any(|t| t == "euros")
        && note.tags.iter().any(|t| t == "q3-2026");

    let ok = note.title == "Sprintplan #euros"
        && headings == 1
        && list_items == 3
        && nested
        && has_tags;

    serial_println!(
        "[an] EuroNotes: titel=\"{}\", {} blokken, koppen={} lijstitems={} (genest={}), tags={:?} {}",
        note.title,
        note.blocks.len(),
        headings,
        list_items,
        nested,
        note.tags,
        if ok { "✓" } else { "✗ FOUT" }
    );
}
