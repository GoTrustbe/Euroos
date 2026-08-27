//! Kernel side of **EuroNotes** (Sprint AC-1): the notes app.
//! At boot we prove the Markdown→EuroDoc pipeline: headings, inline formatting,
//! lists with levels, and `#tag` extraction. Host-tested core: [`euronotes`].
//! Also contains the desktop GUI (`render`): a note list + the selected
//! note rendered by the REAL `euronotes` engine (no mock text).

use crate::graphics::{Color, FrameBuffer};
use crate::serial_println;
use crate::text;
use core::sync::atomic::{AtomicUsize, Ordering};
use eurodoc::model::Block;
use eurofs::fs as eurofs_api;

/// Equal to `compositor::TITLEBAR_H`.
const TITLEBAR_H: usize = 44;

/// Seeded notes (real Markdown). The GUI parses them live with `euronotes`.
const NOTES: &[&str] = &[
    "# Welcome to EuroNotes #euros\n\n\
     This is a **sample** note. The parser is real (from-scratch *euronotes*, \
     Markdown → EuroDoc); the notes themselves are seeded read-only samples.\n\n\
     What works:\n\n\
     - Headings and inline formatting\n\
     - Lists with levels\n  - like this nested line\n\
     - `#tag` extraction\n\n\
     > Sovereignty by design.\n",
    "# Sprint plan AG #roadmap\n\n\
     Breadth sprint after the Zero-Trust cycle:\n\n\
     - AG-1 desktop apps #now\n\
     - AG-2 browser: images + forms\n\
     - AG-3 installer execution\n\
     - AG-4 coreutils long tail\n\n\
     Status: **in progress** and *on track*.\n",
    "# Groceries #home\n\n\
     - Bread\n\
     - Belgian chocolate\n\
     - Coffee\n\n\
     Don't forget: it is *sovereignly* delicious.\n",
];

static SELECTED: AtomicUsize = AtomicUsize::new(0);

/// The LIVE notes (editable). Seeded from `NOTES` on first load, then persisted
/// on EuroFS under /home/euro/notes/note-<i>.md — a notes app that cannot make
/// or edit a note is a viewer wearing the wrong name (UX audit, 2026-08-27).
static LIVE: spin::Mutex<alloc::vec::Vec<alloc::string::String>> =
    spin::Mutex::new(alloc::vec::Vec::new());
static LOADED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static DIRTY: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

const NOTES_DIR: &str = "/home/euro/notes";

/// Load the notes from EuroFS (or seed + persist the samples on first run).
pub fn load(fs: &mut dyn eurofs_api::FileSystem) {
    if LOADED.swap(true, Ordering::Relaxed) {
        return;
    }
    let mut v = alloc::vec::Vec::new();
    for i in 0..64 {
        let path = alloc::format!("{NOTES_DIR}/note-{i}.md");
        match fs.read_file(&path) {
            Ok(b) => v.push(alloc::string::String::from_utf8_lossy(&b).into_owned()),
            Err(_) => break,
        }
    }
    if v.is_empty() {
        v = NOTES.iter().map(|n| alloc::string::String::from(*n)).collect();
        let _ = fs.create_dir(NOTES_DIR);
        for (i, n) in v.iter().enumerate() {
            let _ = fs.write_file(&alloc::format!("{NOTES_DIR}/note-{i}.md"), n.as_bytes());
        }
    }
    *LIVE.lock() = v;
}

/// Persist every note (called after edits; EuroFS writes are cheap in-cache).
pub fn save_all(fs: &mut dyn eurofs_api::FileSystem) {
    let v = LIVE.lock().clone();
    let _ = fs.create_dir(NOTES_DIR);
    for (i, n) in v.iter().enumerate() {
        let _ = fs.write_file(&alloc::format!("{NOTES_DIR}/note-{i}.md"), n.as_bytes());
    }
    DIRTY.store(false, Ordering::Relaxed);
}

/// Was there an edit since the last save_all()?
pub fn take_dirty() -> bool {
    DIRTY.swap(false, Ordering::Relaxed)
}

/// Append a fresh note and select it.
pub fn new_note() {
    let mut v = LIVE.lock();
    v.push(alloc::string::String::from("# New note\n\n"));
    SELECTED.store(v.len() - 1, Ordering::Relaxed);
    DIRTY.store(true, Ordering::Relaxed);
}

/// One key into the SELECTED note (insertion at the end, same as EuroText).
pub fn input(ch: char) {
    let mut v = LIVE.lock();
    let i = SELECTED.load(Ordering::Relaxed).min(v.len().saturating_sub(1));
    let Some(n) = v.get_mut(i) else { return };
    match ch {
        '\r' | '\n' => n.push('\n'),
        '\u{8}' | '\u{7f}' => {
            n.pop();
        }
        c if c >= ' ' => n.push(c),
        _ => return,
    }
    DIRTY.store(true, Ordering::Relaxed);
}

/// The current notes as owned strings (render/selftest).
fn live_notes() -> alloc::vec::Vec<alloc::string::String> {
    let v = LIVE.lock();
    if v.is_empty() {
        NOTES.iter().map(|n| alloc::string::String::from(*n)).collect()
    } else {
        v.clone()
    }
}

/// Which note is open.
pub fn selected() -> usize {
    SELECTED.load(Ordering::Relaxed).min(live_notes().len().saturating_sub(1))
}

/// Click in the note list? Set the selection and return `true` if it changed.
pub fn hit_test(win_x: usize, win_y: usize, mx: usize, my: usize) -> bool {
    let lx = win_x;
    let ly = win_y + TITLEBAR_H + 44; // below the list header
    let list_w = 210usize;
    if mx < lx || mx >= lx + list_w {
        return false;
    }
    let row_h = 50usize;
    if my < ly {
        return false;
    }
    let i = (my - ly) / row_h;
    let notes = live_notes();
    if i < notes.len() {
        let prev = SELECTED.swap(i, Ordering::Relaxed);
        return prev != i;
    }
    // The row right below the last note is the "+ New note" target.
    if i == notes.len() {
        new_note();
        return true;
    }
    false
}

/// Desktop GUI: the note list on the left, the selected note on the right as the
/// `euronotes` engine parses it.
pub fn render(fb: &FrameBuffer, x: usize, y: usize, w: usize, h: usize) {
    let bx = x;
    let by = y + TITLEBAR_H;
    let bw = w;
    let bh = h.saturating_sub(TITLEBAR_H);
    let accent = Color::rgb(0xD6, 0x96, 0x2A); // amber (notes accent)
    fb.fill_rect(bx, by, bw, bh, Color::SURFACE);

    // ── List panel on the left ──────────────────────────────────────────────
    let list_w = 210usize;
    fb.fill_rect(bx, by, list_w, bh, Color::CARD);
    fb.fill_rect(bx + list_w, by, 1, bh, Color::BORDER);
    let live = live_notes();
    text::draw_px(fb, bx + 16, by + 16, "Notes  \u{00B7}  yours, editable", Color::INK, 14.0);
    let cnt = alloc::format!("{}", live.len());
    text::draw_px(fb, bx + list_w - text::width_px(&cnt, 12.0) - 16, by + 18, &cnt, Color::TEXT_DIM, 12.0);

    let sel = selected();
    let row_h = 50usize;
    let row_y0 = by + 44;
    for (i, md) in live.iter().enumerate() {
        let ry = row_y0 + i * row_h;
        let note = euronotes::parse(md);
        if i == sel {
            fb.fill_rounded_rect(bx + 8, ry, list_w - 16, row_h - 6, 9, Color::SURFACE);
            fb.fill_rounded_rect(bx + 8, ry, 3, row_h - 6, 2, accent);
        }
        let title = clip(&note.title, list_w - 40, 13.0);
        text::draw_px(fb, bx + 18, ry + 8, &title, Color::INK, 13.0);
        // First tag as chip text + block count.
        let sub = if let Some(t) = note.tags.first() {
            alloc::format!("#{}  ·  {} blocks", t, note.blocks.len())
        } else {
            alloc::format!("{} blocks", note.blocks.len())
        };
        text::draw_px(fb, bx + 18, ry + 28, &clip(&sub, list_w - 36, 11.0), Color::TEXT_DIM, 11.0);
    }

    // "+ New note" target: the row right after the last note (hit_test matches).
    {
        let ry = row_y0 + live.len() * row_h;
        if ry + 30 < by + bh {
            fb.fill_rounded_rect(bx + 8, ry, list_w - 16, row_h - 12, 9, Color::SURFACE);
            text::draw_px(fb, bx + 18, ry + 10, "+ New note", accent, 13.0);
        }
    }

    // ── Note canvas on the right ─────────────────────────────────────────────
    let src = &live[sel.min(live.len() - 1)];
    let note = euronotes::parse(src);
    let px = bx + list_w + 1;
    let pw = bw - list_w - 1;
    let margin = 30usize;
    let tx = px + margin;
    let maxw = pw.saturating_sub(margin * 2);
    let mut ty = by + 28;

    text::draw_px(fb, tx, ty, &clip(&note.title, maxw, 26.0), Color::INK, 26.0);
    ty += 38;
    fb.fill_rect(tx, ty, 56, 3, accent);
    ty += 18;

    let ymax = by + bh - 44;
    for blk in &note.blocks {
        if ty > ymax {
            break;
        }
        if let Block::Paragraph(p) = blk {
            let txt = p.plain_text();
            let (size, col, indent, bullet) = match p.props.style_id.as_deref() {
                Some("Heading1") => (19.0f32, Color::INK, 0usize, false),
                Some("Heading2") => (16.0, accent, 0, false),
                Some("Quote") => (13.5, Color::TEXT_SEC, 14, false),
                _ => match p.props.list_level {
                    Some(lvl) => (13.5, Color::INK, 14 + lvl as usize * 18, true),
                    None => (13.5, Color::INK, 0, false),
                },
            };
            if txt.trim().is_empty() {
                continue;
            }
            if bullet {
                fb.fill_rounded_rect(tx + indent, ty + 7, 5, 5, 2, accent);
                ty = draw_wrapped(fb, tx + indent + 12, ty, maxw.saturating_sub(indent + 12), &txt, col, size, 20, ymax);
            } else {
                ty = draw_wrapped(fb, tx + indent, ty, maxw.saturating_sub(indent), &txt, col, size, (size as usize) + 8, ymax);
            }
            ty += 6;
        }
    }

    // Tag chips + status bar at the bottom.
    let sy = by + bh - 30;
    fb.fill_rect(px, sy, pw, 30, Color::CARD);
    fb.fill_rect(px, sy, pw, 1, Color::BORDER);
    let mut chx = px + margin;
    for t in &note.tags {
        let label = alloc::format!("#{t}");
        let cw = text::width_px(&label, 11.5) + 18;
        fb.fill_rounded_rect(chx, sy + 6, cw, 18, 9, Color::ACCENT_SOFT);
        text::draw_px(fb, chx + 9, sy + 8, &label, accent, 11.5);
        chx += cw + 8;
        if chx > px + pw - 40 {
            break;
        }
    }
}

/// Draw `s` with simple word wrap; returns the new y.
fn draw_wrapped(fb: &FrameBuffer, x: usize, mut y: usize, maxw: usize, s: &str, col: Color, size: f32, lead: usize, ymax: usize) -> usize {
    use alloc::string::String;
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

/// Clip text to a pixel width with ellipsis.
fn clip(s: &str, maxw: usize, size: f32) -> alloc::string::String {
    use alloc::string::String;
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

/// Boot self-test: parse a note and check the title, blocks and tags.
pub fn selftest() {
    let md = "# Sprint plan #euros\n\n\
              Goals for #q3-2026:\n\n\
              - EuroWeb engine\n\
              - EuroReken\n  - bitwise mode\n\n\
              Status is **good** and *stable*.\n\n\
              > Sovereignty by design.\n";
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

    let ok = note.title == "Sprint plan #euros"
        && headings == 1
        && list_items == 3
        && nested
        && has_tags;

    serial_println!(
        "[an] EuroNotes: title=\"{}\", {} blocks, headings={} list items={} (nested={}), tags={:?} {}",
        note.title,
        note.blocks.len(),
        headings,
        list_items,
        nested,
        note.tags,
        if ok { "✓" } else { "✗ FAIL" }
    );
}
