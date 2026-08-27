//! Tooltips: the little label that appears when the pointer rests on a control.
//! The compositor decides what the cursor is over and, after a short dwell, sets
//! the tooltip text; this module draws it near the cursor. A small thing users
//! only notice when it is missing.

use alloc::string::{String, ToString};
use spin::Mutex;

use crate::graphics::{Color, FrameBuffer};

static TIP: Mutex<Option<(String, usize, usize)>> = Mutex::new(None);

pub fn set(text: &str, x: usize, y: usize) {
    *TIP.lock() = Some((text.to_string(), x, y));
}

pub fn clear() {
    *TIP.lock() = None;
}

pub fn is_shown() -> bool {
    TIP.lock().is_some()
}

pub fn render(fb: &FrameBuffer, screen_w: usize, screen_h: usize) {
    let g = TIP.lock();
    let Some((text, cx, cy)) = g.as_ref() else { return };
    let tw = crate::text::width_px(text, 12.0);
    let w = tw + 20;
    let h = 26;
    // Place below-right of the cursor, clamped on screen.
    let x = (*cx + 16).min(screen_w.saturating_sub(w + 4));
    let y = (*cy + 20).min(screen_h.saturating_sub(h + 4));
    fb.fill_rounded_rect(x + 1, y + 2, w, h, crate::eds::RADIUS_S, Color::BORDER);
    fb.fill_rounded_rect(x, y, w, h, crate::eds::RADIUS_S, Color::INK);
    crate::text::draw_px(fb, x + 10, y + 6, text, Color::rgb(0xF5, 0xF7, 0xFA), 12.0);
}

/// `[tip]` boot self-test: set shows a tooltip, clear removes it.
pub fn selftest() {
    clear();
    let empty = !is_shown();
    set("EuroFiles", 100, 100);
    let shown = is_shown();
    clear();
    let cleared = !is_shown();
    let name_ok = crate::launcher::name_for_icon(4) == Some("Terminal");
    let ok = empty && shown && cleared && name_ok;
    crate::serial_println!(
        "[tip] Tooltips: set-shows={shown}, clear-hides={cleared}, dock-names={name_ok} → {}",
        if ok { "OK (hover labels on controls) ✓" } else { "FAILED ✗" }
    );
}
