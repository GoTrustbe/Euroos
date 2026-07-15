//! The Alt-Tab application switcher: hold Alt, tap Tab to cycle through the open
//! windows in most-recently-used order; release Alt to raise the highlighted
//! one. Shows a centered strip of window titles while active.

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use spin::Mutex;

use crate::graphics::{Color, FrameBuffer};

static OPEN: AtomicBool = AtomicBool::new(false);
static SEL: AtomicUsize = AtomicUsize::new(0);
static LIST: Mutex<Vec<(usize, String)>> = Mutex::new(Vec::new());

pub fn is_open() -> bool {
    OPEN.load(Ordering::Relaxed)
}

/// Start switching over `items` = (window index, title), MRU order. Selection
/// starts on the second entry (the previous window), the usual Alt-Tab default.
pub fn begin(items: Vec<(usize, String)>) {
    let n = items.len();
    *LIST.lock() = items;
    SEL.store(if n > 1 { 1 } else { 0 }, Ordering::Relaxed);
    OPEN.store(!LIST.lock().is_empty(), Ordering::Relaxed);
}

/// Advance the highlight to the next window (wraps).
pub fn advance() {
    let n = LIST.lock().len();
    if n > 0 {
        SEL.store((SEL.load(Ordering::Relaxed) + 1) % n, Ordering::Relaxed);
    }
}

/// The window index currently highlighted.
pub fn selected() -> Option<usize> {
    let l = LIST.lock();
    l.get(SEL.load(Ordering::Relaxed)).map(|(i, _)| *i)
}

pub fn close() {
    OPEN.store(false, Ordering::Relaxed);
    LIST.lock().clear();
}

pub fn render(fb: &FrameBuffer, screen_w: usize, screen_h: usize) {
    if !is_open() {
        return;
    }
    let list = LIST.lock();
    if list.is_empty() {
        return;
    }
    let sel = SEL.load(Ordering::Relaxed);
    let row_h = 40usize;
    let w = 420usize;
    let h = 44 + list.len() * row_h + 12;
    let x = (screen_w.saturating_sub(w)) / 2;
    let y = (screen_h.saturating_sub(h)) / 2;
    fb.fill_rounded_rect(x + 1, y + 3, w, h, crate::eds::RADIUS_L, Color::BORDER);
    fb.fill_rounded_rect(x, y, w, h, crate::eds::RADIUS_L, Color::CARD);
    fb.draw_border(x, y, w, h, 1, Color::BORDER);
    crate::text::draw_px(fb, x + 20, y + 14, "Switch window", Color::TEXT_DIM, 12.0);
    let mut cy = y + 40;
    for (i, (_, title)) in list.iter().enumerate() {
        if i == sel {
            fb.fill_rounded_rect(x + 10, cy, w - 20, row_h - 6, crate::eds::RADIUS_S, Color::ACCENT_SOFT);
        }
        let c = if i == sel { Color::ACCENT } else { Color::INK };
        crate::text::draw_px(fb, x + 24, cy + 8, title, c, 14.0);
        cy += row_h;
    }
}

/// `[altab]` boot self-test: the switcher cycles and wraps, starting on the
/// previous window.
pub fn selftest() {
    begin(alloc::vec![
        (5, String::from("Terminal")),
        (2, String::from("Files")),
        (7, String::from("Notes")),
    ]);
    let opened = is_open();
    let starts_prev = selected() == Some(2); // second entry
    advance();
    let stepped = selected() == Some(7);
    advance();
    let wrapped = selected() == Some(5);
    close();
    let closed = !is_open();
    let ok = opened && starts_prev && stepped && wrapped && closed;
    crate::serial_println!(
        "[altab] App switcher: opens={opened}, starts-on-previous={starts_prev}, cycles={stepped}, wraps={wrapped} → {}",
        if ok { "OK (Alt-Tab between windows) ✓" } else { "FAILED ✗" }
    );
}
