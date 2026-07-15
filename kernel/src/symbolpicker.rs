//! Symbol/emoji picker: a small palette of characters to drop into a text
//! field. Colour emoji need a colour font the kernel does not ship, so this is
//! an honest "insert symbol" palette of glyphs the built-in font renders
//! (arrows, maths, currency, marks). Opened from the text context menu.

use alloc::string::{String, ToString};
use spin::Mutex;

use crate::graphics::{Color, FrameBuffer};

/// The palette (font-renderable glyphs, not colour emoji).
const SYMBOLS: &[&str] = &[
    "→", "←", "↑", "↓", "↔", "⇒",
    "•", "◦", "‣", "…", "·", "°",
    "✓", "✗", "★", "☆", "♥", "♦",
    "€", "£", "¥", "©", "®", "™",
    "×", "÷", "±", "≈", "≠", "≤",
    "≥", "∞", "µ", "π", "Σ", "√",
];

const COLS: usize = 6;
const CELL: usize = 46;
static OPEN: Mutex<bool> = Mutex::new(false);

pub fn is_open() -> bool {
    *OPEN.lock()
}
pub fn open() {
    *OPEN.lock() = true;
}
pub fn close() {
    *OPEN.lock() = false;
}

fn geom(screen_w: usize, screen_h: usize) -> (usize, usize, usize, usize) {
    let rows = SYMBOLS.len().div_ceil(COLS);
    let w = COLS * CELL + 24;
    let h = rows * CELL + 52;
    ((screen_w.saturating_sub(w)) / 2, (screen_h.saturating_sub(h)) / 2, w, h)
}

/// A click: returns the chosen symbol (and closes) or dismisses on outside click.
pub fn click_at(mx: usize, my: usize, screen_w: usize, screen_h: usize) -> Option<String> {
    if !is_open() {
        return None;
    }
    let (x, y, w, h) = geom(screen_w, screen_h);
    if mx < x || mx >= x + w || my < y || my >= y + h {
        close();
        return None;
    }
    let gx = x + 12;
    let gy = y + 44;
    if mx >= gx && my >= gy {
        let col = (mx - gx) / CELL;
        let row = (my - gy) / CELL;
        if col < COLS {
            let idx = row * COLS + col;
            if idx < SYMBOLS.len() {
                close();
                return Some(SYMBOLS[idx].to_string());
            }
        }
    }
    None
}

pub fn render(fb: &FrameBuffer, screen_w: usize, screen_h: usize) {
    if !is_open() {
        return;
    }
    let (x, y, w, h) = geom(screen_w, screen_h);
    fb.fill_rounded_rect(x + 1, y + 3, w, h, crate::eds::RADIUS_L, Color::BORDER);
    fb.fill_rounded_rect(x, y, w, h, crate::eds::RADIUS_L, Color::CARD);
    fb.draw_border(x, y, w, h, 1, Color::BORDER);
    crate::text::draw_px(fb, x + 16, y + 14, "Insert symbol", Color::INK, 15.0);
    let gx = x + 12;
    let gy = y + 44;
    for (i, s) in SYMBOLS.iter().enumerate() {
        let cx = gx + (i % COLS) * CELL;
        let cy = gy + (i / COLS) * CELL;
        fb.fill_rounded_rect(cx + 2, cy + 2, CELL - 4, CELL - 4, crate::eds::RADIUS_S, Color::SURFACE);
        crate::text::draw_px(fb, cx + 14, cy + 12, s, Color::INK, 18.0);
    }
}

/// `[sym]` boot self-test: the palette opens, a cell click returns a symbol.
pub fn selftest() {
    open();
    let opened = is_open();
    // Click the first cell (top-left of the grid) on a 1920x1080 screen.
    let (x, y, _, _) = geom(1920, 1080);
    let pick = click_at(x + 12 + 10, y + 44 + 10, 1920, 1080);
    let got = pick.as_deref() == Some(SYMBOLS[0]);
    let closed = !is_open();
    let ok = opened && got && closed;
    crate::serial_println!(
        "[symbol] Symbol picker: opens={opened}, pick-returns-symbol={got}, closes={closed} → {}",
        if ok { "OK (insert symbols into text) ✓" } else { "FAILED ✗" }
    );
}
