//! **EuroText** (Sprint 4) — a REAL plain-text editor: typing, backspace,
//! new lines, and SAVING to the real EuroFS (and reading back after reopening).
//! No mock: the content is written out as a file and survives a restart.
//!
//! Limitation (honest): the PS/2 driver provides no arrow keys/Ctrl, so the cursor sits
//! at the insertion point (typewriter style: type/​backspace/​enter); saving happens
//! via the "Save" button in the toolbar (mouse). The edit-+-save-+-reload cycle
//! is host-independently verified via the `[edit]` self-test.

use crate::graphics::{Color, FrameBuffer};
use crate::{serial_println, text};
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use eurofs::FileSystem;
use spin::Mutex;

const TITLEBAR_H: usize = 44;
const TOOLBAR_H: usize = 40;
const LINE_H: usize = 20;
const PAD: usize = 14;
const DEFAULT_PATH: &str = "/home/euro/euro.txt";

struct Editor {
    buf: crate::editcore::Buffer,
    path: String,
    status: String,
    scroll: usize, // first visible row
}

static ED: Mutex<Option<Editor>> = Mutex::new(None);

fn ed<'a>(g: &'a mut Option<Editor>) -> &'a mut Editor {
    if g.is_none() {
        *g = Some(Editor {
            buf: crate::editcore::Buffer::new(),
            path: String::new(),
            status: String::new(),
            scroll: 0,
        });
    }
    g.as_mut().unwrap()
}

/// Open `path` (or the default file) in the editor; if it does not exist, we start
/// with a welcome text. Fills the line buffer from the REAL file content.
pub fn open(fs: &mut dyn FileSystem, path: &str) {
    let p = if path.is_empty() { DEFAULT_PATH } else { path };
    let content = match fs.read_file(p) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(_) => String::from("Welcome to EuroText.\nType here; click 'Save' to write to EuroFS.\n"),
    };
    let mut g = ED.lock();
    let e = ed(&mut g);
    e.buf.set_text(&content);
    e.path = String::from(p);
    e.scroll = 0;
    e.status = alloc::format!("opened: {p}");
}

/// A raw character (paste, or the symbol picker) inserted at the cursor.
pub fn input(ch: char) {
    let k = match ch {
        '\r' | '\n' => crate::ps2::Key::Enter,
        '\u{8}' | '\u{7f}' => crate::ps2::Key::Backspace,
        '\t' => crate::ps2::Key::Tab,
        c => crate::ps2::Key::Char(c),
    };
    key(k);
}

/// A rich key (from poll_key_ex): navigation, edit, or a character.
pub fn key(k: crate::ps2::Key) {
    let mut g = ED.lock();
    let e = ed(&mut g);
    if e.buf.key(k) {
        e.status = if e.buf.dirty { String::from("unsaved changes") } else { e.status.clone() };
    }
}

/// Move the text cursor to the character nearest a click at window-local (mx,my).
pub fn click(win_x: usize, win_y: usize, mx: usize, my: usize) -> bool {
    let mut g = ED.lock();
    let e = ed(&mut g);
    let tx = win_x + PAD;
    let ty0 = win_y + TITLEBAR_H + TOOLBAR_H + 8;
    if my < ty0 || mx < tx {
        return false;
    }
    let rrow = e.scroll + (my - ty0) / LINE_H;
    if rrow >= e.buf.lines.len() {
        return false;
    }
    e.buf.row = rrow;
    // Column: widest prefix whose pixel width fits the click x.
    let line = e.buf.lines[rrow].clone();
    let mut col = 0;
    let mut acc = tx;
    for ch in line.chars() {
        let mut b = [0u8; 4];
        let cw = text::width_px(ch.encode_utf8(&mut b), 15.0);
        if acc + cw / 2 >= mx {
            break;
        }
        acc += cw;
        col += 1;
    }
    e.buf.col = col;
    true
}

/// Write the buffer to EuroFS (real, durable). Returns `true` on success.
pub fn save(fs: &mut dyn FileSystem) -> bool {
    let (path, body) = {
        let mut g = ED.lock();
        let e = ed(&mut g);
        (e.path.clone(), e.buf.text())
    };
    let path = if path.is_empty() { DEFAULT_PATH.to_string() } else { path };
    // Make sure the directory exists.
    if let Some(slash) = path.rfind('/') {
        if slash > 0 {
            let _ = fs.create_dir(&path[..slash]);
        }
    }
    let ok = fs.write_file(&path, body.as_bytes()).is_ok();
    let mut g = ED.lock();
    let e = ed(&mut g);
    e.buf.dirty = !ok;
    e.status = if ok {
        alloc::format!("saved → {path} ({} B)", body.len())
    } else {
        alloc::format!("SAVE FAILED → {path}")
    };
    serial_println!("[edit] save {} → {}", path, if ok { "OK" } else { "FAILED" });
    ok
}

/// Save the current buffer to a chosen path (Save-As, from the file dialog).
pub fn save_to(fs: &mut dyn FileSystem, path: &str) -> bool {
    { let mut g = ED.lock(); ed(&mut g).path = path.to_string(); }
    save(fs)
}

/// Rectangle of the "Save" button in the toolbar (for the mouse hit test).
fn save_button(x: usize, y: usize, w: usize) -> (usize, usize, usize, usize) {
    let bw = 96;
    let bx = x + w.saturating_sub(bw + PAD);
    let by = y + TITLEBAR_H + 6;
    (bx, by, bw, TOOLBAR_H - 12)
}

/// Was the "Save" button clicked?
pub fn save_button_at(x: usize, y: usize, w: usize, mx: usize, my: usize) -> bool {
    let (bx, by, bw, bh) = save_button(x, y, w);
    mx >= bx && mx < bx + bw && my >= by && my < by + bh
}

/// Rectangle of the "Open" button, just left of Save.
fn open_button(x: usize, y: usize, w: usize) -> (usize, usize, usize, usize) {
    let bw = 96;
    let (sx, by, _sw, bh) = save_button(x, y, w);
    (sx.saturating_sub(bw + 8), by, bw, bh)
}

/// Was the "Open" button clicked?
pub fn open_button_at(x: usize, y: usize, w: usize, mx: usize, my: usize) -> bool {
    let (bx, by, bw, bh) = open_button(x, y, w);
    mx >= bx && mx < bx + bw && my >= by && my < by + bh
}

pub fn render(fb: &FrameBuffer, x: usize, y: usize, w: usize, h: usize) {
    let bx = x;
    let by = y + TITLEBAR_H;
    let bw = w;
    let bh = h.saturating_sub(TITLEBAR_H);
    let mut g = ED.lock();
    let e = ed(&mut g);
    let accent = Color::rgb(0x2B, 0x6C, 0xB0);
    let ink = Color::rgb(0x20, 0x24, 0x2C);
    let gutter = 44usize; // line-number column

    // Toolbar.
    fb.fill_rect(bx, by, bw, TOOLBAR_H, Color::rgb(0xF1, 0xF3, 0xF7));
    let dot = if e.buf.dirty { "\u{25CF} " } else { "" };
    text::draw_px(fb, bx + PAD, by + 11, &alloc::format!("{dot}{}", e.path), ink, 15.0);
    // Open button (left of Save).
    let (ox, oy, ow, oh) = open_button(x, y, w);
    fb.fill_rounded_rect(ox, oy, ow, oh, 6, Color::rgb(0xE9, 0xED, 0xF3));
    fb.draw_border(ox, oy, ow, oh, 1, Color::rgb(0xCF, 0xD6, 0xE0));
    text::draw_px(fb, ox + 16, oy + 5, "Open", ink, 14.0);
    // Save button.
    let (sx, sy, sw, sh) = save_button(x, y, w);
    fb.fill_rounded_rect(sx, sy, sw, sh, 6, accent);
    text::draw_px(fb, sx + 16, sy + 5, "Save", Color::rgb(0xFF, 0xFF, 0xFF), 14.0);

    // Text area with a line-number gutter and a real cursor.
    let tx = bx + PAD + gutter;
    let ty0 = by + TOOLBAR_H + 8;
    let maxrows = bh.saturating_sub(TOOLBAR_H + 8 + 24) / LINE_H;
    let total = e.buf.lines.len();
    // Keep the cursor row on screen (scroll to follow it).
    if e.buf.row < e.scroll {
        e.scroll = e.buf.row;
    } else if e.buf.row >= e.scroll + maxrows {
        e.scroll = e.buf.row + 1 - maxrows;
    }
    // Gutter background.
    fb.fill_rect(bx, ty0 - 4, PAD + gutter - 6, bh, Color::rgb(0xF4, 0xF6, 0xFA));
    let start = e.scroll;
    for vis in 0..maxrows {
        let i = start + vis;
        if i >= total {
            break;
        }
        let line = &e.buf.lines[i];
        let ty = ty0 + vis * LINE_H;
        // Line number.
        let num = alloc::format!("{}", i + 1);
        text::draw_px(fb, bx + PAD, ty, &num, Color::rgb(0xA8, 0xB0, 0xBC), 12.5);
        text::draw_px(fb, tx, ty, line, ink, 15.0);
        for (sp, l) in crate::spell::misspellings(line) {
            if sp + l <= line.len() {
                let x0 = tx + text::width_px(&line[..sp], 15.0);
                let ww = text::width_px(&line[sp..sp + l], 15.0);
                fb.fill_rect(x0, ty + LINE_H - 4, ww, 2, Color::rgb(0xD0, 0x3A, 0x3A));
            }
        }
        // The cursor: a caret bar at (row,col).
        if i == e.buf.row {
            let prefix: String = line.chars().take(e.buf.col).collect();
            let cx = tx + text::width_px(&prefix, 15.0);
            fb.fill_rect(cx, ty, 2, LINE_H - 2, accent);
        }
    }

    // Status bar.
    let sb_y = by + bh.saturating_sub(22);
    fb.fill_rect(bx, sb_y, bw, 22, Color::rgb(0xEC, 0xEF, 0xF4));
    text::draw_px(
        fb,
        bx + PAD,
        sb_y + 4,
        &alloc::format!("EuroText \u{00B7} {} lines \u{00B7} ln {}, col {} \u{00B7} {}", total, e.buf.row + 1, e.buf.col + 1, e.status),
        Color::rgb(0x55, 0x5C, 0x68),
        12.5,
    );
}

/// **[edit]** — prove the edit-→-save-→-reload cycle on the REAL EuroFS,
/// independent of keyboard/mouse: type text in, save, reload, compare.
pub fn selftest(fs: &mut dyn FileSystem) {
    let path = "/tmp/edit-selftest.txt";
    open(fs, path); // fresh buffer (file does not exist → welcome text)
    // Replace the buffer with controlled content via input().
    { let mut g = ED.lock(); ed(&mut g).buf.set_text(""); }
    for ch in "Hello".chars() {
        input(ch);
    }
    input('\r');
    for ch in "EuroOS 42".chars() {
        input(ch);
    }
    input('\u{8}'); // backspace: "42" → "4"
    let saved = save(fs);
    // Reload from a fresh open() — proves it is really on disk.
    open(fs, path);
    let reread = { let mut g = ED.lock(); ed(&mut g).buf.text() };
    let expected = "Hello\nEuroOS 4";
    let ok = saved && reread == expected;
    let _ = fs.remove_file(path);
    serial_println!(
        "[edit] EuroText edit+save+reread: write-OK={saved}, reread={:?} (expected {:?}) → {}",
        reread,
        expected,
        if ok { "OK ✓" } else { "FAILED ✗" }
    );
}
