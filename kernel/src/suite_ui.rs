//! EuroSuite GUI (BB-5): Word/Excel/PowerPoint-like rendering of the three apps
//! (Writer/Calc/Impress) in the compositor, on top of the EuroDoc UDM + the EuroCalc
//! formula engine. One rich render function per app: a colored ribbon bar
//! (ribbon) with tabs + buttons, a document canvas, and a status bar.

use crate::graphics::{Color, FrameBuffer};
use crate::{icons, text};
use alloc::string::String;
use alloc::vec::Vec;
use eurodoc::model::{Block, Body, Cell, SheetBody, Slide};
use eurodoc::Document;

/// Which EuroSuite app a window shows (None = plain text/terminal window).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SuiteApp {
    None,
    Writer,
    Calc,
    Impress,
    /// EuroWeb browser: renders a real HTML+CSS page via the in-house engine.
    Browser,
    /// EuroReken: a REAL interactive calculator (state in `win.content`).
    Reken,
    /// EuroBeheer: settings/management — shows and manages the LIVE kernel state.
    Settings,
    /// EuroAgent: dispatch panel — intent → agent loop → live cap-gated tool calls.
    Agent,
    /// EuroInstall: guided graphical installer (plan + live FDE enrollment).
    Installer,
    /// EuroFiles: file manager — shows the LIVE EuroFS.
    Files,
    /// EuroNotes: note-taking app — real Markdown via the euronotes engine.
    Notes,
    /// EuroClock: world clocks + local time from the REAL RTC.
    Clock,
    /// EuroText: plain-text editor — edits + REALLY saves to EuroFS.
    Text,
    /// EuroMonitor: live system monitor (RAM/tasks/disk/audit — real measurements).
    Monitor,
    /// EuroLog: live view of the hash-chained audit log.
    Log,
    /// A hosted X11 client (e.g. a real GTK app): its window body is the live pixel
    /// buffer from the in-kernel X server, composited as a framed desktop window.
    XClient,
}

const TITLEBAR_H: usize = 44; // must equal compositor::TITLEBAR_H
const RIBBON_H: usize = 64;
const STATUS_H: usize = 26;

/// Draw the body of a EuroSuite window (everything below the title bar).
pub fn render(fb: &FrameBuffer, x: usize, y: usize, w: usize, h: usize, app: SuiteApp) {
    let bx = x;
    let by = y + TITLEBAR_H;
    let bw = w;
    let bh = h.saturating_sub(TITLEBAR_H);
    match app {
        SuiteApp::Writer => writer(fb, bx, by, bw, bh),
        SuiteApp::Calc => calc(fb, bx, by, bw, bh),
        SuiteApp::Impress => impress(fb, bx, by, bw, bh),
        // Browser/Reken/Settings are already handled before this dispatch (they read
        // their own state: fetched HTML / calculator / live kernel state).
        SuiteApp::Browser => {}
        SuiteApp::Reken => {}
        SuiteApp::Settings => {}
        SuiteApp::Agent => {}
        SuiteApp::Installer => {}
        // Files/Notes/Clock read their own state and are handled before this
        // dispatch (see compositor::draw_window_body).
        SuiteApp::Files => {}
        SuiteApp::Notes => {}
        SuiteApp::Clock => {}
        // EuroText/EuroMonitor/EuroLog read their own state and are handled before this
        // dispatch (see compositor::draw_window_body).
        SuiteApp::Text => {}
        SuiteApp::Monitor => {}
        SuiteApp::Log => {}
        // XClient body is the hosted X-server pixel buffer, drawn before this dispatch.
        SuiteApp::XClient => {}
        SuiteApp::None => {}
    }
}

// ── Shared chrome ────────────────────────────────────────────────

/// A ribbon bar: app accent color, tab labels and a row of tool buttons.
fn ribbon(fb: &FrameBuffer, x: usize, y: usize, w: usize, accent: Color, tabs: &[&str], active_tab: usize, tools: &[(&str, &str)]) {
    fb.fill_rect(x, y, w, RIBBON_H, Color::CARD);
    // Tab row.
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
    // Separator line below the tabs.
    fb.fill_rect(x, y + 34, w, 1, Color::BORDER);
    // Tool buttons (icon tiles).
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

/// A status bar at the bottom with left- and right-aligned text.
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
        b.push(Block::Paragraph(eurodoc::model::Paragraph::text("Sovereignty by design").styled("Heading1")));
        b.push(Block::Paragraph(eurodoc::model::Paragraph::text(
            "EuroOS is the first fully sovereign European operating system, built from scratch in Rust. This is a read-only sample document rendered by EuroSuite Writer (editing/export not yet wired).")));
        b.push(Block::Paragraph(eurodoc::model::Paragraph::text("Core principles").styled("Heading2")));
        b.push(Block::Paragraph(
            eurodoc::model::Paragraph::new()
                .run(eurodoc::model::Run::new("No inherited code").bold())
                .run(eurodoc::model::Run::new(" — every layer is original: kernel, file system, network, TLS and this word processor.")),
        ));
        b.push(Block::Paragraph(eurodoc::model::Paragraph::text(
            "The trust boundary lives in the kernel, not in a cloud. AI agents run capability-isolated and fully offline.")));
    }
    d
}

fn writer(fb: &FrameBuffer, x: usize, y: usize, w: usize, h: usize) {
    let accent = Color::ACCENT;
    ribbon(fb, x, y, w, accent, &["Home", "Insert", "Layout", "Review"], 0,
        &[("doc", "B"), ("doc", "I"), ("doc", "U"), ("rect", "EuroSans 12"), ("grid", "Styles")]);

    // Document worksheet: gray background, a white 'page' centered with a shadow.
    let work_y = y + RIBBON_H + 1;
    let work_h = h.saturating_sub(RIBBON_H + 1 + STATUS_H);
    fb.fill_rect(x, work_y, w, work_h, Color::SURFACE_3);

    let page_w = (w * 78 / 100).min(w.saturating_sub(48));
    let page_x = x + (w - page_w) / 2;
    let page_y = work_y + 18;
    let page_h = work_h.saturating_sub(34);
    fb.drop_shadow(page_x, page_y, page_w, page_h, 10, 4, Color::rgb(0x1A, 0x22, 0x2C));
    fb.fill_rect(page_x, page_y, page_w, page_h, Color::SURFACE);

    // Render the paragraphs with margins; headings larger/bolder.
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
                // Simple word wrap.
                ty = draw_wrapped(fb, tx, ty, maxw, &p.plain_text(), col, size, lead, page_y + page_h - 30);
                ty += 8;
            }
            if ty > page_y + page_h - 24 {
                break;
            }
        }
    }
    let words = doc.word_count();
    statusbar(fb, x, y, w, h, accent, "Page 1 of 1   ·   Dutch (nl-BE)",
        &alloc::format!("{words} words   ·   sample document (read-only preview)   ·   100%"));
}

/// Draw `s` with simple word wrap; returns the new y.
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
    s.set(0, 0, txt("Region"));
    s.set(0, 1, txt("Q1"));
    s.set(0, 2, txt("Q2"));
    s.set(0, 3, txt("Total"));
    s.set(1, 0, txt("Belgium"));
    s.set(1, 1, num(1200));
    s.set(1, 2, num(1450));
    s.set(1, 3, Cell::Formula(String::from("=B2+C2")));
    s.set(2, 0, txt("Netherlands"));
    s.set(2, 1, num(2100));
    s.set(2, 2, num(1980));
    s.set(2, 3, Cell::Formula(String::from("=B3+C3")));
    s.set(3, 0, txt("Germany"));
    s.set(3, 1, num(3400));
    s.set(3, 2, num(3650));
    s.set(3, 3, Cell::Formula(String::from("=B4+C4")));
    s.set(4, 0, txt("Sum"));
    s.set(4, 3, Cell::Formula(String::from("=SUM(D2:D4)")));
    s
}

fn calc(fb: &FrameBuffer, x: usize, y: usize, w: usize, h: usize) {
    let accent = Color::SUCCESS;
    ribbon(fb, x, y, w, accent, &["Home", "Insert", "Formulas", "Data"], 2,
        &[("grid", "Σ Sum"), ("rect", "Currency"), ("plus", "Chart"), ("doc", "B")]);

    let sheet = demo_sheet();
    // Formula bar.
    let fb_y = y + RIBBON_H + 1;
    fb.fill_rect(x, fb_y, w, 24, Color::SURFACE);
    fb.fill_rounded_rect(x + 10, fb_y + 3, 54, 18, 5, Color::SURFACE_3);
    text::draw_px(fb, x + 20, fb_y + 5, "D5", Color::INK, 11.5);
    text::draw_px(fb, x + 76, fb_y + 5, "=SUM(D2:D4)", Color::TEXT_SEC, 11.5);
    fb.fill_rect(x, fb_y + 24, w, 1, Color::BORDER);

    // Grid.
    let grid_y = fb_y + 25;
    let grid_h = h.saturating_sub(RIBBON_H + 1 + 25 + STATUS_H + 24); // -24 for sheet tabs
    let rowh = 26usize;
    let hdr = 22usize;
    let rownum_w = 38usize;
    let ncols = 5usize;
    let colw = (w.saturating_sub(rownum_w)) / ncols;

    // Column headers A B C D E.
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
        // Row number.
        fb.fill_rect(x, ry, rownum_w, rowh, Color::SURFACE_3);
        let rs = alloc::format!("{}", r + 1);
        text::draw_px(fb, x + 12, ry + 6, &rs, Color::TEXT_SEC, 11.0);
        fb.fill_rect(x, ry + rowh, w, 1, Color::BORDER);
        // Cells.
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
    // Highlight the selected cell D5 (row 4, column 3).
    let sel_x = x + rownum_w + 3 * colw;
    let sel_y = grid_y + hdr + 4 * rowh;
    fb.draw_border(sel_x, sel_y, colw, rowh, 2, accent);

    // Sheet tabs.
    let tab_y = y + h - STATUS_H - 24;
    fb.fill_rect(x, tab_y, w, 24, Color::CARD);
    fb.fill_rounded_rect(x + 10, tab_y + 3, 60, 18, 5, Color::SURFACE);
    text::draw_px(fb, x + 20, tab_y + 5, "Sheet 1", Color::INK, 11.5);
    text::draw_px(fb, x + 82, tab_y + 5, "Sheet 2", Color::TEXT_SEC, 11.5);
    icons::draw(fb, "plus", x + 140, tab_y + 5, 14, Color::TEXT_SEC);

    let total = eurocalc::eval("=SUM(D2:D4)", &sheet).unwrap_or(0.0) as i64;
    statusbar(fb, x, y, w, h, accent, "Ready   ·   nl-BE",
        &alloc::format!("Sum: {total}   ·   sample workbook (live formula engine)   ·   100%"));
}

// ── Impress ──────────────────────────────────────────────────────────────────

fn demo_deck() -> Vec<Slide> {
    let s = |title: &str, body: &str| Slide {
        title: String::from(title),
        blocks: alloc::vec![Block::Paragraph(eurodoc::model::Paragraph::text(body))],
    };
    alloc::vec![
        s("EuroOS", "The sovereign European operating system"),
        s("Sovereign by design", "Own code · own keys · hosted in Europe"),
        s("AI agents in the kernel", "Capability-isolated · fully offline"),
        s("Thank you", "euro-os.eu"),
    ]
}

fn impress(fb: &FrameBuffer, x: usize, y: usize, w: usize, h: usize) {
    let accent = Color::rgb(0xC0, 0x52, 0x2C); // warm orange (PowerPoint-like)
    ribbon(fb, x, y, w, accent, &["Home", "Insert", "Design", "Slide Show"], 0,
        &[("plus", "New slide"), ("rect", "Layout"), ("grid", "Theme"), ("doc", "B")]);

    let deck = demo_deck();
    let work_y = y + RIBBON_H + 1;
    let work_h = h.saturating_sub(RIBBON_H + 1 + STATUS_H);
    fb.fill_rect(x, work_y, w, work_h, Color::SURFACE_3);

    // Thumbnail strip on the left.
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
        // mini title
        text::draw_px(fb, thx + 8, thy + 8, &clip(&sl.title, thumb_w - 14, 9.0), Color::INK, 9.0);
        thy += thumb_h + 16;
        if thy > work_y + work_h - thumb_h {
            break;
        }
    }

    // Main slide canvas (16:9), centered in the remaining space.
    let area_x = x + strip_w + 1;
    let area_w = w - strip_w - 1;
    let slide_w = (area_w * 86 / 100).min(area_w.saturating_sub(40));
    let slide_h = slide_w * 9 / 16;
    let slide_x = area_x + (area_w - slide_w) / 2;
    let slide_y = work_y + (work_h.saturating_sub(slide_h)) / 2;
    fb.drop_shadow(slide_x, slide_y, slide_w, slide_h, 12, 5, Color::rgb(0x1A, 0x22, 0x2C));
    // Slide background with an accent band at the top.
    fb.fill_rect(slide_x, slide_y, slide_w, slide_h, Color::SURFACE);
    fb.fill_rect(slide_x, slide_y, slide_w, 8, accent);
    // Title + content of slide 1.
    let s0 = &deck[0];
    text::draw_px(fb, slide_x + 44, slide_y + slide_h / 2 - 40, &s0.title, Color::INK, 40.0);
    if let Some(Block::Paragraph(p)) = s0.blocks.first() {
        text::draw_px(fb, slide_x + 44, slide_y + slide_h / 2 + 16, &p.plain_text(), Color::TEXT_SEC, 18.0);
    }
    // Accent stripe below the title.
    fb.fill_rect(slide_x + 44, slide_y + slide_h / 2 + 4, 90, 4, accent);

    statusbar(fb, x, y, w, h, accent, "Slide 1 of 4   ·   nl-BE",
        "Sample deck (preview)   ·   only slide 1 renders   ·   not yet interactive");
}

/// Truncate text to a pixel width with an ellipsis.
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
