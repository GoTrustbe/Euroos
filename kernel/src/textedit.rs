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
    lines: Vec<String>,
    path: String,
    dirty: bool,
    status: String,
}

static ED: Mutex<Editor> = Mutex::new(Editor {
    lines: Vec::new(),
    path: String::new(),
    dirty: false,
    status: String::new(),
});

/// Open `path` (or the default file) in the editor; if it does not exist, we start
/// with a welcome text. Fills the line buffer from the REAL file content.
pub fn open(fs: &mut dyn FileSystem, path: &str) {
    let p = if path.is_empty() { DEFAULT_PATH } else { path };
    let content = match fs.read_file(p) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(_) => String::from("Welcome to EuroText.\nType here; click 'Save' to write to EuroFS.\n"),
    };
    let mut lines: Vec<String> = content.split('\n').map(String::from).collect();
    if lines.is_empty() {
        lines.push(String::new());
    }
    let mut ed = ED.lock();
    ed.lines = lines;
    ed.path = String::from(p);
    ed.dirty = false;
    ed.status = alloc::format!("opened: {p}");
}

/// Process one key at the insertion point (end of the buffer).
pub fn input(ch: char) {
    let mut ed = ED.lock();
    if ed.lines.is_empty() {
        ed.lines.push(String::new());
    }
    match ch {
        '\r' | '\n' => ed.lines.push(String::new()),
        '\u{8}' | '\u{7f}' => {
            // Backspace: remove last character, or merge lines.
            if let Some(last) = ed.lines.last_mut() {
                if last.pop().is_none() && ed.lines.len() > 1 {
                    ed.lines.pop();
                }
            }
        }
        '\t' => {
            if let Some(last) = ed.lines.last_mut() {
                last.push_str("    ");
            }
        }
        c if !c.is_control() => {
            if let Some(last) = ed.lines.last_mut() {
                last.push(c);
            }
        }
        _ => return,
    }
    ed.dirty = true;
    ed.status = "unsaved changes".to_string();
}

/// Write the buffer to EuroFS (real, durable). Returns `true` on success.
pub fn save(fs: &mut dyn FileSystem) -> bool {
    let (path, body) = {
        let ed = ED.lock();
        (ed.path.clone(), ed.lines.join("\n"))
    };
    let path = if path.is_empty() { DEFAULT_PATH.to_string() } else { path };
    // Make sure the directory exists.
    if let Some(slash) = path.rfind('/') {
        if slash > 0 {
            let _ = fs.create_dir(&path[..slash]);
        }
    }
    let ok = fs.write_file(&path, body.as_bytes()).is_ok();
    let mut ed = ED.lock();
    ed.dirty = !ok;
    ed.status = if ok {
        alloc::format!("saved → {path} ({} B)", body.len())
    } else {
        alloc::format!("SAVE FAILED → {path}")
    };
    serial_println!("[edit] save {} → {}", path, if ok { "OK" } else { "FAILED" });
    ok
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

pub fn render(fb: &FrameBuffer, x: usize, y: usize, w: usize, h: usize) {
    let bx = x;
    let by = y + TITLEBAR_H;
    let bw = w;
    let bh = h.saturating_sub(TITLEBAR_H);
    let ed = ED.lock();
    let accent = Color::rgb(0x2B, 0x6C, 0xB0);
    let ink = Color::rgb(0x20, 0x24, 0x2C);

    // Toolbar.
    fb.fill_rect(bx, by, bw, TOOLBAR_H, Color::rgb(0xF1, 0xF3, 0xF7));
    let dot = if ed.dirty { "● " } else { "" };
    text::draw_px(fb, bx + PAD, by + 11, &alloc::format!("{dot}{}", ed.path), ink, 15.0);
    // Save button.
    let (sx, sy, sw, sh) = save_button(x, y, w);
    fb.fill_rounded_rect(sx, sy, sw, sh, 6, accent);
    text::draw_px(fb, sx + 16, sy + 5, "Save", Color::rgb(0xFF, 0xFF, 0xFF), 14.0);

    // Text area.
    let tx = bx + PAD;
    let mut ty = by + TOOLBAR_H + 8;
    let maxrows = bh.saturating_sub(TOOLBAR_H + 8 + 24) / LINE_H;
    let total = ed.lines.len();
    let start = total.saturating_sub(maxrows);
    for (i, line) in ed.lines.iter().enumerate().skip(start) {
        let last = i + 1 == total;
        let shown = if last {
            alloc::format!("{line}_") // caret at the insertion point
        } else {
            line.clone()
        };
        text::draw_px(fb, tx, ty, &shown, ink, 15.0);
        ty += LINE_H;
    }

    // Status bar.
    let sb_y = by + bh.saturating_sub(22);
    fb.fill_rect(bx, sb_y, bw, 22, Color::rgb(0xEC, 0xEF, 0xF4));
    text::draw_px(
        fb,
        bx + PAD,
        sb_y + 4,
        &alloc::format!("EuroText · {} lines · {}", total, ed.status),
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
    {
        let mut ed = ED.lock();
        ed.lines = vec![String::new()];
    }
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
    let reread = ED.lock().lines.join("\n");
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
