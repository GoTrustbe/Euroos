//! EuroSuite-GUI (BB-5): Word-/Excel-/PowerPoint-achtige rendering van de drie apps
//! (Writer/Calc/Impress) in de compositor, bovenop het EuroDoc-UDM + de EuroCalc-
//! formule-engine. Eén rijke render-functie per app: een gekleurde lint-balk
//! (ribbon) met tabs + knoppen, een document-canvas, en een statusbalk.

use crate::graphics::{Color, FrameBuffer};
use crate::{icons, text};
use alloc::string::String;
use alloc::vec::Vec;
use eurodoc::model::{Block, Body, Cell, SheetBody, Slide};
use eurodoc::Document;

/// Welke EuroSuite-app een venster toont (None = gewoon tekst/terminal-venster).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SuiteApp {
    None,
    Writer,
    Calc,
    Impress,
    /// EuroWeb-browser: rendert een echte HTML+CSS-pagina via de eigen engine.
    Browser,
    /// EuroReken: een ECHTE interactieve rekenmachine (toestand in `win.content`).
    Reken,
    /// EuroBeheer: instellingen/beheer — toont en beheert de LIVE kernel-toestand.
    Settings,
    /// EuroAgent: dispatch-paneel — intent → agent-lus → live cap-gated tool-calls.
    Agent,
    /// EuroInstall: begeleide grafische installer (plan + live FDE-enrol).
    Installer,
    /// EuroFiles: bestandsbeheerder — toont het LIVE EuroFS.
    Files,
    /// EuroNotes: notitie-app — echte Markdown via de euronotes-engine.
    Notes,
    /// EuroClock: wereldklokken + lokale tijd uit de ECHTE RTC.
    Clock,
    /// EuroText: platte-tekst-editor — bewerkt + slaat ECHT op naar EuroFS.
    Text,
    /// EuroMonitor: live systeemmonitor (RAM/taken/schijf/audit — echte metingen).
    Monitor,
    /// EuroLog: live weergave van het hash-geketende audit-logboek.
    Log,
}

const TITLEBAR_H: usize = 44; // moet gelijk zijn aan compositor::TITLEBAR_H
const RIBBON_H: usize = 64;
const STATUS_H: usize = 26;

/// Teken de body van een EuroSuite-venster (alles ónder de titelbalk).
pub fn render(fb: &FrameBuffer, x: usize, y: usize, w: usize, h: usize, app: SuiteApp) {
    let bx = x;
    let by = y + TITLEBAR_H;
    let bw = w;
    let bh = h.saturating_sub(TITLEBAR_H);
    match app {
        SuiteApp::Writer => writer(fb, bx, by, bw, bh),
        SuiteApp::Calc => calc(fb, bx, by, bw, bh),
        SuiteApp::Impress => impress(fb, bx, by, bw, bh),
        // Browser/Reken/Settings worden al vóór deze dispatch afgehandeld (ze lezen
        // hun eigen toestand: opgehaalde HTML / rekenmachine / live kernel-state).
        SuiteApp::Browser => {}
        SuiteApp::Reken => {}
        SuiteApp::Settings => {}
        SuiteApp::Agent => {}
        SuiteApp::Installer => {}
        // Files/Notes/Clock lezen hun eigen toestand en worden vóór deze dispatch
        // afgehandeld (zie compositor::draw_window_body).
        SuiteApp::Files => {}
        SuiteApp::Notes => {}
        SuiteApp::Clock => {}
        // EuroText/EuroMonitor/EuroLog lezen hun eigen toestand en worden vóór deze
        // dispatch afgehandeld (zie compositor::draw_window_body).
        SuiteApp::Text => {}
        SuiteApp::Monitor => {}
        SuiteApp::Log => {}
        SuiteApp::None => {}
    }
}

// ── Gemeenschappelijke chrome ────────────────────────────────────────────────

/// Een lintbalk (ribbon): app-accentkleur, tab-labels en een rij tool-knoppen.
fn ribbon(fb: &FrameBuffer, x: usize, y: usize, w: usize, accent: Color, tabs: &[&str], active_tab: usize, tools: &[(&str, &str)]) {
    fb.fill_rect(x, y, w, RIBBON_H, Color::CARD);
    // Tab-rij.
    let mut tx = x + 16;
    for (i, t) in tabs.iter().enumerate() {
        let tw = text::width_px(t, 12.5) + 18;
        if i == active_tab {
            fb.fill_rounded_rect(tx - 6, y + 6, tw, 22, 7, accent);
            text::draw_px(fb, tx, y + 11, t, Color::SURFACE, 12.5);
        } else {
            text::draw_px(fb, tx, y + 11, t, Color::TEXT_SEC, 12.5);
        }
        tx += tw + 6;
    }
    // Scheidingslijn onder de tabs.
    fb.fill_rect(x, y + 34, w, 1, Color::BORDER);
    // Tool-knoppen (icoon-tegels).
    let mut gx = x + 14;
    let gy = y + 40;
    for (icon, label) in tools {
        let lw = if label.is_empty() { 0 } else { text::width_px(label, 11.0) + 4 };
        let btn_w = 22 + lw + 12;
        fb.fill_rounded_rect(gx, gy, btn_w, 20, 6, Color::SURFACE_3);
        icons::draw(fb, icon, gx + 6, gy + 3, 14, Color::INK);
        if !label.is_empty() {
            text::draw_px(fb, gx + 24, gy + 4, label, Color::INK, 11.0);
        }
        gx += btn_w + 7;
    }
    fb.fill_rect(x, y + RIBBON_H, w, 1, Color::BORDER);
}

/// Een statusbalk onderaan met links- en rechts-uitgelijnde tekst.
fn statusbar(fb: &FrameBuffer, x: usize, y: usize, w: usize, h: usize, accent: Color, left: &str, right: &str) {
    let sy = y + h - STATUS_H;
    fb.fill_rect(x, sy, w, STATUS_H, accent);
    text::draw_px(fb, x + 14, sy + 6, left, Color::SURFACE, 11.5);
    let rw = text::width_px(right, 11.5);
    text::draw_px(fb, x + w - rw - 14, sy + 6, right, Color::SURFACE, 11.5);
}

// ── Writer ───────────────────────────────────────────────────────────────────

fn demo_writer_doc() -> Document {
    let mut d = Document::writer();
    d.metadata.language = String::from("nl-BE");
    if let Body::Writer(b) = &mut d.body {
        b.push(Block::Paragraph(eurodoc::model::Paragraph::text("Soevereiniteit door ontwerp").styled("Heading1")));
        b.push(Block::Paragraph(eurodoc::model::Paragraph::text(
            "EuroOS is het eerste volledig soevereine Europese besturingssysteem, van nul gebouwd in Rust. Dit document is geopend in EuroSuite Writer en kan als .docx of .odt bewaard worden.")));
        b.push(Block::Paragraph(eurodoc::model::Paragraph::text("Kernprincipes").styled("Heading2")));
        b.push(Block::Paragraph(
            eurodoc::model::Paragraph::new()
                .run(eurodoc::model::Run::new("Geen geërfde code").bold())
                .run(eurodoc::model::Run::new(" — elke laag is origineel: kernel, bestandssysteem, netwerk, TLS en deze tekstverwerker.")),
        ));
        b.push(Block::Paragraph(eurodoc::model::Paragraph::text(
            "De vertrouwensgrens zit in de kernel, niet in een cloud. AI-agents draaien capability-geïsoleerd en volledig offline.")));
    }
    d
}

fn writer(fb: &FrameBuffer, x: usize, y: usize, w: usize, h: usize) {
    let accent = Color::ACCENT;
    ribbon(fb, x, y, w, accent, &["Start", "Invoegen", "Indeling", "Controleren"], 0,
        &[("doc", "B"), ("doc", "I"), ("doc", "U"), ("rect", "EuroSans 12"), ("grid", "Stijlen")]);

    // Documentwerkblad: grijze achtergrond, een witte 'pagina' gecentreerd met schaduw.
    let work_y = y + RIBBON_H + 1;
    let work_h = h.saturating_sub(RIBBON_H + 1 + STATUS_H);
    fb.fill_rect(x, work_y, w, work_h, Color::SURFACE_3);

    let page_w = (w * 78 / 100).min(w.saturating_sub(48));
    let page_x = x + (w - page_w) / 2;
    let page_y = work_y + 18;
    let page_h = work_h.saturating_sub(34);
    fb.drop_shadow(page_x, page_y, page_w, page_h, 10, 4, Color::rgb(0x1A, 0x22, 0x2C));
    fb.fill_rect(page_x, page_y, page_w, page_h, Color::SURFACE);

    // Render de paragrafen met marges; koppen groter/vetter.
    let doc = demo_writer_doc();
    let margin = 40;
    let mut ty = page_y + 36;
    let tx = page_x + margin;
    let maxw = page_w.saturating_sub(margin * 2);
    if let Body::Writer(blocks) = &doc.body {
        for blk in blocks {
            if let Block::Paragraph(p) = blk {
                let (size, col, lead) = match p.props.style_id.as_deref() {
                    Some("Heading1") => (24.0f32, Color::INK, 34),
                    Some("Heading2") => (17.0, Color::ACCENT, 26),
                    _ => (13.5, Color::INK, 20),
                };
                // Eenvoudige woord-wrap.
                ty = draw_wrapped(fb, tx, ty, maxw, &p.plain_text(), col, size, lead, page_y + page_h - 30);
                ty += 8;
            }
            if ty > page_y + page_h - 24 {
                break;
            }
        }
    }
    let words = doc.word_count();
    statusbar(fb, x, y, w, h, accent, "Pagina 1 van 1   ·   Nederlands (nl-BE)",
        &alloc::format!("{words} woorden   ·   .docx / .odt   ·   100%"));
}

/// Teken `s` met simpele woord-wrap; geeft de nieuwe y terug.
fn draw_wrapped(fb: &FrameBuffer, x: usize, mut y: usize, maxw: usize, s: &str, col: Color, size: f32, lead: usize, ymax: usize) -> usize {
    let mut line = String::new();
    for word in s.split(' ') {
        let trial = if line.is_empty() { String::from(word) } else { alloc::format!("{line} {word}") };
        if text::width_px(&trial, size) > maxw && !line.is_empty() {
            text::draw_px(fb, x, y, &line, col, size);
            y += lead;
            line = String::from(word);
            if y > ymax {
                return y;
            }
        } else {
            line = trial;
        }
    }
    if !line.is_empty() {
        text::draw_px(fb, x, y, &line, col, size);
        y += lead;
    }
    y
}

// ── Calc ───────────────────────────────────────────────────────────────────

fn demo_sheet() -> SheetBody {
    let mut s = SheetBody::default();
    let txt = |t: &str| Cell::Text(String::from(t));
    let num = |n: i64| Cell::Number { scaled: n, scale: 0 };
    s.set(0, 0, txt("Regio"));
    s.set(0, 1, txt("Q1"));
    s.set(0, 2, txt("Q2"));
    s.set(0, 3, txt("Totaal"));
    s.set(1, 0, txt("België"));
    s.set(1, 1, num(1200));
    s.set(1, 2, num(1450));
    s.set(1, 3, Cell::Formula(String::from("=B2+C2")));
    s.set(2, 0, txt("Nederland"));
    s.set(2, 1, num(2100));
    s.set(2, 2, num(1980));
    s.set(2, 3, Cell::Formula(String::from("=B3+C3")));
    s.set(3, 0, txt("Duitsland"));
    s.set(3, 1, num(3400));
    s.set(3, 2, num(3650));
    s.set(3, 3, Cell::Formula(String::from("=B4+C4")));
    s.set(4, 0, txt("Som"));
    s.set(4, 3, Cell::Formula(String::from("=SUM(D2:D4)")));
    s
}

fn calc(fb: &FrameBuffer, x: usize, y: usize, w: usize, h: usize) {
    let accent = Color::SUCCESS;
    ribbon(fb, x, y, w, accent, &["Start", "Invoegen", "Formules", "Gegevens"], 2,
        &[("grid", "Σ Som"), ("rect", "Valuta"), ("plus", "Grafiek"), ("doc", "B")]);

    let sheet = demo_sheet();
    // Formulebalk.
    let fb_y = y + RIBBON_H + 1;
    fb.fill_rect(x, fb_y, w, 24, Color::SURFACE);
    fb.fill_rounded_rect(x + 10, fb_y + 3, 54, 18, 5, Color::SURFACE_3);
    text::draw_px(fb, x + 20, fb_y + 5, "D5", Color::INK, 11.5);
    text::draw_px(fb, x + 76, fb_y + 5, "=SUM(D2:D4)", Color::TEXT_SEC, 11.5);
    fb.fill_rect(x, fb_y + 24, w, 1, Color::BORDER);

    // Grid.
    let grid_y = fb_y + 25;
    let grid_h = h.saturating_sub(RIBBON_H + 1 + 25 + STATUS_H + 24); // -24 voor bladtabs
    let rowh = 26usize;
    let hdr = 22usize;
    let rownum_w = 38usize;
    let ncols = 5usize;
    let colw = (w.saturating_sub(rownum_w)) / ncols;

    // Kolomkoppen A B C D E.
    fb.fill_rect(x, grid_y, w, hdr, Color::SURFACE_3);
    fb.fill_rect(x, grid_y, rownum_w, hdr, Color::SURFACE_3);
    for c in 0..ncols {
        let cx = x + rownum_w + c * colw;
        let letter = (b'A' + c as u8) as char;
        let s: String = letter.into();
        text::draw_px(fb, cx + colw / 2 - 4, grid_y + 4, &s, Color::TEXT_SEC, 11.5);
        fb.fill_rect(cx, grid_y, 1, grid_h, Color::BORDER);
    }
    fb.fill_rect(x + rownum_w, grid_y, 1, grid_h, Color::BORDER);

    let nrows = (grid_h.saturating_sub(hdr)) / rowh;
    for r in 0..nrows {
        let ry = grid_y + hdr + r * rowh;
        // Rijnummer.
        fb.fill_rect(x, ry, rownum_w, rowh, Color::SURFACE_3);
        let rs = alloc::format!("{}", r + 1);
        text::draw_px(fb, x + 12, ry + 6, &rs, Color::TEXT_SEC, 11.0);
        fb.fill_rect(x, ry + rowh, w, 1, Color::BORDER);
        // Cellen.
        for c in 0..ncols {
            let cx = x + rownum_w + c * colw;
            let val = sheet.get(r as u32, c as u32);
            let (shown, right_align, head) = match &val {
                Cell::Empty => (String::new(), false, false),
                Cell::Text(t) => (t.clone(), false, r == 0),
                Cell::Number { scaled, .. } => (alloc::format!("{scaled}"), true, false),
                Cell::Formula(f) => {
                    let v = eurocalc::eval(f, &sheet).map(|n| alloc::format!("{}", n as i64)).unwrap_or_else(|_| String::from("#ERR"));
                    (v, true, r == 4)
                }
            };
            if r == 0 {
                fb.fill_rect(cx, ry, colw, rowh, Color::ACCENT_SOFT);
            }
            if !shown.is_empty() {
                let col = if head { Color::ACCENT } else { Color::INK };
                if right_align {
                    let tw = text::width_px(&shown, 12.0);
                    text::draw_px(fb, cx + colw.saturating_sub(tw + 10), ry + 6, &shown, col, 12.0);
                } else {
                    text::draw_px(fb, cx + 8, ry + 6, &shown, col, 12.0);
                }
            }
        }
    }
    // Geselecteerde cel D5 highlighten (rij 4, kolom 3).
    let sel_x = x + rownum_w + 3 * colw;
    let sel_y = grid_y + hdr + 4 * rowh;
    fb.draw_border(sel_x, sel_y, colw, rowh, 2, accent);

    // Bladtabs.
    let tab_y = y + h - STATUS_H - 24;
    fb.fill_rect(x, tab_y, w, 24, Color::CARD);
    fb.fill_rounded_rect(x + 10, tab_y + 3, 60, 18, 5, Color::SURFACE);
    text::draw_px(fb, x + 20, tab_y + 5, "Blad 1", Color::INK, 11.5);
    text::draw_px(fb, x + 82, tab_y + 5, "Blad 2", Color::TEXT_SEC, 11.5);
    icons::draw(fb, "plus", x + 140, tab_y + 5, 14, Color::TEXT_SEC);

    let total = eurocalc::eval("=SUM(D2:D4)", &sheet).unwrap_or(0.0) as i64;
    statusbar(fb, x, y, w, h, accent, "Klaar   ·   nl-BE",
        &alloc::format!("Som: {total}   ·   Cellen: 17   ·   .xlsx / .ods   ·   100%"));
}

// ── Impress ──────────────────────────────────────────────────────────────────

fn demo_deck() -> Vec<Slide> {
    let s = |title: &str, body: &str| Slide {
        title: String::from(title),
        blocks: alloc::vec![Block::Paragraph(eurodoc::model::Paragraph::text(body))],
    };
    alloc::vec![
        s("EuroOS", "Het soevereine Europese besturingssysteem"),
        s("Soeverein van ontwerp", "Eigen code · eigen sleutels · gehost in Europa"),
        s("AI-agents in de kernel", "Capability-geïsoleerd · volledig offline"),
        s("Bedankt", "euro-os.eu"),
    ]
}

fn impress(fb: &FrameBuffer, x: usize, y: usize, w: usize, h: usize) {
    let accent = Color::rgb(0xC0, 0x52, 0x2C); // warm oranje (PowerPoint-achtig)
    ribbon(fb, x, y, w, accent, &["Start", "Invoegen", "Ontwerp", "Diavoorstelling"], 0,
        &[("plus", "Nieuwe dia"), ("rect", "Indeling"), ("grid", "Thema"), ("doc", "B")]);

    let deck = demo_deck();
    let work_y = y + RIBBON_H + 1;
    let work_h = h.saturating_sub(RIBBON_H + 1 + STATUS_H);
    fb.fill_rect(x, work_y, w, work_h, Color::SURFACE_3);

    // Thumbnail-strip links.
    let strip_w = 150usize;
    fb.fill_rect(x, work_y, strip_w, work_h, Color::CARD);
    fb.fill_rect(x + strip_w, work_y, 1, work_h, Color::BORDER);
    let thumb_w = strip_w - 28;
    let thumb_h = thumb_w * 9 / 16;
    let mut thy = work_y + 14;
    for (i, sl) in deck.iter().enumerate() {
        let thx = x + 14;
        if i == 0 {
            fb.draw_border(thx - 3, thy - 3, thumb_w + 6, thumb_h + 6, 2, accent);
        }
        fb.fill_rect(thx, thy, thumb_w, thumb_h, Color::SURFACE);
        fb.draw_border(thx, thy, thumb_w, thumb_h, 1, Color::BORDER);
        let num = alloc::format!("{}", i + 1);
        text::draw_px(fb, thx - 1, thy + thumb_h / 2 - 6, &num, Color::TEXT_DIM, 10.0);
        // mini-titel
        text::draw_px(fb, thx + 8, thy + 8, &clip(&sl.title, thumb_w - 14, 9.0), Color::INK, 9.0);
        thy += thumb_h + 16;
        if thy > work_y + work_h - thumb_h {
            break;
        }
    }

    // Hoofd-dia-canvas (16:9), gecentreerd in de resterende ruimte.
    let area_x = x + strip_w + 1;
    let area_w = w - strip_w - 1;
    let slide_w = (area_w * 86 / 100).min(area_w.saturating_sub(40));
    let slide_h = slide_w * 9 / 16;
    let slide_x = area_x + (area_w - slide_w) / 2;
    let slide_y = work_y + (work_h.saturating_sub(slide_h)) / 2;
    fb.drop_shadow(slide_x, slide_y, slide_w, slide_h, 12, 5, Color::rgb(0x1A, 0x22, 0x2C));
    // Dia-achtergrond met een accentband bovenaan.
    fb.fill_rect(slide_x, slide_y, slide_w, slide_h, Color::SURFACE);
    fb.fill_rect(slide_x, slide_y, slide_w, 8, accent);
    // Titel + inhoud van dia 1.
    let s0 = &deck[0];
    text::draw_px(fb, slide_x + 44, slide_y + slide_h / 2 - 40, &s0.title, Color::INK, 40.0);
    if let Some(Block::Paragraph(p)) = s0.blocks.first() {
        text::draw_px(fb, slide_x + 44, slide_y + slide_h / 2 + 16, &p.plain_text(), Color::TEXT_SEC, 18.0);
    }
    // Accentstreep onder de titel.
    fb.fill_rect(slide_x + 44, slide_y + slide_h / 2 + 4, 90, 4, accent);

    statusbar(fb, x, y, w, h, accent, "Dia 1 van 4   ·   nl-BE",
        "Presentatie   ·   .pptx / .odp   ·   Klik om te presenteren");
}

/// Knip tekst af op pixelbreedte met een ellipsis.
fn clip(s: &str, maxw: usize, size: f32) -> String {
    if text::width_px(s, size) <= maxw {
        return String::from(s);
    }
    let mut out = String::new();
    for ch in s.chars() {
        let trial = alloc::format!("{out}{ch}…");
        if text::width_px(&trial, size) > maxw {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}
