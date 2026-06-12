//! Kernel-zijde van **EuroSuite** (ES-Core/IO/Writer/Calc/Impress): het soevereine
//! kantoorpakket. Bij boot bewijzen we de hele stapel — een Writer-document door de
//! OOXML-round-trip + HTML-export, de Calc-formule-engine, en een Impress-presentatie
//! — allemaal op het ene EuroDoc-UDM. Host-geteste kern: [`eurodoc`]/[`eurodocio`]/
//! [`eurocalc`].

use alloc::string::String;
use alloc::vec::Vec;

use eurodoc::model::{Block, Body, Cell, Paragraph, Run, SheetBody, Slide};
use eurodoc::Document;

/// Boot-zelftest: Writer (OOXML round-trip + HTML), Calc (formules), Impress (dia's).
pub fn selftest() {
    // ── Writer: document → OOXML → terug, behoudt opmaak; HTML-export. ──
    let mut doc = Document::writer();
    if let Body::Writer(b) = &mut doc.body {
        b.push(Block::Paragraph(Paragraph::text("EuroSuite Writer").styled("Heading1")));
        b.push(Block::Paragraph(Paragraph::new().run(Run::new("Vet").bold()).run(Run::new(" en gewoon."))));
    }
    let words_ok = doc.word_count() == 5; // "EuroSuite Writer Vet en gewoon."
    let blocks = if let Body::Writer(b) = &doc.body { b.clone() } else { Vec::new() };
    let docx = eurodocio::ooxml::write_body(&blocks);
    let reparsed = eurodocio::ooxml::parse_body(&docx);
    let roundtrip_ok = match reparsed.get(1) {
        Some(Block::Paragraph(p)) => p.runs.first().map(|r| r.props.bold).unwrap_or(false) && p.plain_text() == "Vet en gewoon.",
        _ => false,
    };
    let html = eurodocio::html::blocks_to_html(&blocks);
    let html_ok = html.contains("<h1>EuroSuite Writer</h1>") && html.contains("<strong>Vet</strong>");

    // ── Calc: rekenblad met formules. ──
    let mut sheet = SheetBody::default();
    sheet.set(0, 0, Cell::Number { scaled: 10, scale: 0 });
    sheet.set(1, 0, Cell::Number { scaled: 20, scale: 0 });
    sheet.set(2, 0, Cell::Number { scaled: 30, scale: 0 });
    sheet.set(3, 0, Cell::Formula(String::from("=SUM(A1:A3)")));
    let calc_ok = eurocalc::eval("=A4*2+MAX(A1:A3)", &sheet) == Ok(150.0)
        && eurocalc::eval("=AVERAGE(A1:A3)", &sheet) == Ok(20.0);
    // Cyclusdetectie.
    let mut cyc = SheetBody::default();
    cyc.set(0, 0, Cell::Formula(String::from("=A2")));
    cyc.set(1, 0, Cell::Formula(String::from("=A1")));
    let cycle_ok = eurocalc::eval("=A1", &cyc) == Err(eurocalc::CalcError::Cycle);

    // ── Impress: presentatie met dia's. ──
    let mut deck = Document::deck();
    if let Body::Deck(s) = &mut deck.body {
        s.push(Slide { title: String::from("Welkom bij EuroSuite"), blocks: alloc::vec![Block::Paragraph(Paragraph::text("Soeverein kantoorpakket"))] });
        s.push(Slide { title: String::from("Bedankt"), blocks: alloc::vec![] });
    }
    let deck_ok = deck.element_count() == 2 && deck.plain_text().contains("Welkom bij EuroSuite");

    let ok = words_ok && roundtrip_ok && html_ok && calc_ok && cycle_ok && deck_ok;
    crate::serial_println!(
        "[es] EuroSuite: Writer(OOXML-round-trip={roundtrip_ok}, HTML-export={html_ok}, woordental={words_ok}), Calc(formules+bereiken={calc_ok}, cyclus-detectie={cycle_ok}), Impress(dia's={deck_ok}) → {}",
        if ok { "OK (soeverein kantoorpakket op één Universeel Document Model — OOXML+ODF) ✓" } else { "MISLUKT" }
    );
}

/// `eurosuite`-shellcommando: toon de mogelijkheden + een live Calc-evaluatie.
pub fn shell(args: &str) -> Vec<String> {
    // Optioneel: `eurosuite calc <formule>` evalueert een formule over een demo-blad.
    if let Some(formula) = args.trim().strip_prefix("calc ") {
        let mut sheet = SheetBody::default();
        sheet.set(0, 0, Cell::Number { scaled: 10, scale: 0 });
        sheet.set(1, 0, Cell::Number { scaled: 20, scale: 0 });
        sheet.set(2, 0, Cell::Number { scaled: 30, scale: 0 });
        return match eurocalc::eval(formula, &sheet) {
            Ok(v) => alloc::vec![alloc::format!("{formula} = {v}  (A1=10, A2=20, A3=30)")],
            Err(e) => alloc::vec![alloc::format!("{formula}: fout {e:?}")],
        };
    }
    alloc::vec![
        String::from("EuroSuite — soeverein kantoorpakket op één Universeel Document Model (EuroDoc)"),
        String::from("  Writer  — tekst (OOXML .docx + ODF .odt lezen/schrijven, HTML-export)"),
        String::from("  Calc    — rekenblad met formule-engine (SUM/AVERAGE/MIN/MAX/IF/ROUND, bereiken, cyclusveilig)"),
        String::from("  Impress — presentaties (dia's, outline)"),
        String::from("  alles host-getest; ZIP-container + GUI-rendering koppelen aan eupkg/de compositor"),
        String::from("  probeer: eurosuite calc =SUM(A1:A3)*2"),
    ]
}
