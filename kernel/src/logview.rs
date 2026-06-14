//! **EuroLog** (Sprint 4) — a live view of the sovereign audit log.
//! Shows the REAL, hash-chained security events (login, cap gates,
//! immutability, agent tool calls, …) that the kernel recorded during this session.
//! No mock — `audit::recent()` reads the actual events.

use crate::graphics::{Color, FrameBuffer};
use crate::text;

const TITLEBAR_H: usize = 44;
const LINE_H: usize = 22;

pub fn render(fb: &FrameBuffer, x: usize, y: usize, w: usize, h: usize) {
    let bx = x;
    let by = y + TITLEBAR_H;
    let bw = w;
    let bh = h.saturating_sub(TITLEBAR_H);
    let ink = Color::rgb(0x20, 0x24, 0x2C);
    let dim = Color::rgb(0x60, 0x68, 0x74);
    let accent = Color::rgb(0xB0, 0x4A, 0x2B);

    fb.fill_rect(bx, by, bw, bh, Color::rgb(0xFA, 0xFB, 0xFD));
    let total = crate::audit::count();
    text::draw_px(fb, bx + 18, by + 16, "EuroLog — audit log (hash chain)", ink, 19.0);
    text::draw_px(
        fb,
        bx + 18,
        by + 44,
        &alloc::format!("{total} event(s) this session · immutably chained", ),
        dim,
        13.5,
    );

    let rows = (bh.saturating_sub(80)) / LINE_H;
    let events = crate::audit::recent(rows);
    let mut ty = by + 74;
    for (i, ev) in events.iter().enumerate() {
        let n = total.saturating_sub(events.len()) + i + 1;
        fb.fill_rect(bx + 14, ty + LINE_H / 2, 4, 4, accent);
        text::draw_px(fb, bx + 26, ty, &alloc::format!("#{n:<4} {ev}"), ink, 13.5);
        ty += LINE_H;
    }
    if events.is_empty() {
        text::draw_px(fb, bx + 26, ty, "(no events yet)", dim, 13.5);
    }
}
