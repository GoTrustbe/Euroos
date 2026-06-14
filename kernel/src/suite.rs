//! Kernel side of **EuroSuite** (ES-Core/IO/Writer/Calc/Impress): the sovereign
//! office suite. At boot we prove the whole stack — a Writer document through the
//! OOXML round-trip + HTML export, the Calc formula engine, and an Impress presentation
//! — all on the one EuroDoc UDM. Host-tested core: [`eurodoc`]/[`eurodocio`]/
//! [`eurocalc`].

use alloc::string::String;
use alloc::vec::Vec;

use eurodoc::model::{Block, Body, Cell, Paragraph, Run, SheetBody, Slide};
use eurodoc::Document;

/// Boot self-test: Writer (OOXML round-trip + HTML), Calc (formulas), Impress (slides).
pub fn selftest() {
    // ── Writer: document → OOXML → back, preserves formatting; HTML export. ──
    let mut doc = Document::writer();
    if let Body::Writer(b) = &mut doc.body {
        b.push(Block::Paragraph(Paragraph::text("EuroSuite Writer").styled("Heading1")));
        b.push(Block::Paragraph(Paragraph::new().run(Run::new("Bold").bold()).run(Run::new(" and plain."))));
    }
    let words_ok = doc.word_count() == 5; // "EuroSuite Writer Bold and plain."
    let blocks = if let Body::Writer(b) = &doc.body { b.clone() } else { Vec::new() };
    let docx = eurodocio::ooxml::write_body(&blocks);
    let reparsed = eurodocio::ooxml::parse_body(&docx);
    let roundtrip_ok = match reparsed.get(1) {
        Some(Block::Paragraph(p)) => p.runs.first().map(|r| r.props.bold).unwrap_or(false) && p.plain_text() == "Bold and plain.",
        _ => false,
    };
    let html = eurodocio::html::blocks_to_html(&blocks);
    let html_ok = html.contains("<h1>EuroSuite Writer</h1>") && html.contains("<strong>Bold</strong>");

    // ── Calc: spreadsheet with formulas. ──
    let mut sheet = SheetBody::default();
    sheet.set(0, 0, Cell::Number { scaled: 10, scale: 0 });
    sheet.set(1, 0, Cell::Number { scaled: 20, scale: 0 });
    sheet.set(2, 0, Cell::Number { scaled: 30, scale: 0 });
    sheet.set(3, 0, Cell::Formula(String::from("=SUM(A1:A3)")));
    let calc_ok = eurocalc::eval("=A4*2+MAX(A1:A3)", &sheet) == Ok(150.0)
        && eurocalc::eval("=AVERAGE(A1:A3)", &sheet) == Ok(20.0);
    // Cycle detection.
    let mut cyc = SheetBody::default();
    cyc.set(0, 0, Cell::Formula(String::from("=A2")));
    cyc.set(1, 0, Cell::Formula(String::from("=A1")));
    let cycle_ok = eurocalc::eval("=A1", &cyc) == Err(eurocalc::CalcError::Cycle);

    // ── Impress: presentation with slides. ──
    let mut deck = Document::deck();
    if let Body::Deck(s) = &mut deck.body {
        s.push(Slide { title: String::from("Welcome to EuroSuite"), blocks: alloc::vec![Block::Paragraph(Paragraph::text("Sovereign office suite"))] });
        s.push(Slide { title: String::from("Thank you"), blocks: alloc::vec![] });
    }
    let deck_ok = deck.element_count() == 2 && deck.plain_text().contains("Welcome to EuroSuite");

    let ok = words_ok && roundtrip_ok && html_ok && calc_ok && cycle_ok && deck_ok;
    crate::serial_println!(
        "[es] EuroSuite: Writer(OOXML-round-trip={roundtrip_ok}, HTML-export={html_ok}, word-count={words_ok}), Calc(formulas+ranges={calc_ok}, cycle-detection={cycle_ok}), Impress(slides={deck_ok}) → {}",
        if ok { "OK (sovereign office suite on one Universal Document Model — OOXML+ODF) ✓" } else { "FAILED" }
    );
}

/// `eurosuite` shell command: show the capabilities + a live Calc evaluation.
pub fn shell(args: &str) -> Vec<String> {
    // Optional: `eurosuite calc <formula>` evaluates a formula over a demo sheet.
    if let Some(formula) = args.trim().strip_prefix("calc ") {
        let mut sheet = SheetBody::default();
        sheet.set(0, 0, Cell::Number { scaled: 10, scale: 0 });
        sheet.set(1, 0, Cell::Number { scaled: 20, scale: 0 });
        sheet.set(2, 0, Cell::Number { scaled: 30, scale: 0 });
        return match eurocalc::eval(formula, &sheet) {
            Ok(v) => alloc::vec![alloc::format!("{formula} = {v}  (A1=10, A2=20, A3=30)")],
            Err(e) => alloc::vec![alloc::format!("{formula}: error {e:?}")],
        };
    }
    alloc::vec![
        String::from("EuroSuite — sovereign office suite on one Universal Document Model (EuroDoc)"),
        String::from("  Writer  — text (read/write OOXML .docx + ODF .odt, HTML export)"),
        String::from("  Calc    — spreadsheet with formula engine (SUM/AVERAGE/MIN/MAX/IF/ROUND, ranges, cycle-safe)"),
        String::from("  Impress — presentations (slides, outline)"),
        String::from("  all host-tested; ZIP container + GUI rendering hook into eupkg/the compositor"),
        String::from("  try: eurosuite calc =SUM(A1:A3)*2"),
    ]
}
