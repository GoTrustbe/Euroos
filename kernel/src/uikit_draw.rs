//! EuroUI control painters (`euroui::draw`): the pixels of a toggle, checkbox,
//! slider, text field, dropdown, button, meter and scrollbar. Colors and radii
//! come from the EDS tokens, text goes through `text::draw_px`. No state and no
//! hit-testing here: `euroui::form` decides where a control sits and what a
//! click means; apps normally use that and never call these directly.

use super::form::{Metrics, Rect};
use crate::eds;
use crate::graphics::{Color, FrameBuffer};
use crate::text;

/// Top y for text of size `px` centered in a box at `y` of height `h`.
pub fn text_y(y: usize, h: usize, px: f32) -> usize {
    y + h.saturating_sub(text::line_height(px)) / 2
}

/// Focus ring around a control (keyboard focus must always be visible).
pub fn focus_ring(fb: &FrameBuffer, r: Rect) {
    fb.draw_border(r.x.saturating_sub(3), r.y.saturating_sub(3), r.w + 6, r.h + 6, 2, Color::ACCENT);
}

/// On/off switch. `r` is the track. Green = on; the state is also written next
/// to it by the form, so it never depends on color alone.
pub fn toggle(fb: &FrameBuffer, r: Rect, on: bool, enabled: bool) {
    let track = match (enabled, on) {
        (false, _) => Color::BORDER,
        (true, true) => eds::SEC_VERIFIED,
        (true, false) => Color::TEXT_DIM,
    };
    fb.fill_rounded_rect(r.x, r.y, r.w, r.h, r.h / 2, track);
    let knob = r.h.saturating_sub(6);
    let kx = if on { r.x + r.w.saturating_sub(knob + 3) } else { r.x + 3 };
    fb.fill_rounded_rect(kx, r.y + 3, knob, knob, knob / 2, Color::WHITE);
}

/// Checkbox: an outlined square, filled with a check mark when set.
pub fn checkbox(fb: &FrameBuffer, r: Rect, on: bool, enabled: bool) {
    let edge = if enabled { Color::TEXT_SEC } else { Color::BORDER };
    if on {
        let fill = if enabled { Color::ACCENT } else { Color::TEXT_DIM };
        fb.fill_rounded_rect(r.x, r.y, r.w, r.h, 4, fill);
        let (x, y, s) = (r.x as f32, r.y as f32, r.w as f32);
        fb.aa_seg(x + s * 0.24, y + s * 0.52, x + s * 0.43, y + s * 0.70, 1.0, Color::WHITE);
        fb.aa_seg(x + s * 0.43, y + s * 0.70, x + s * 0.76, y + s * 0.30, 1.0, Color::WHITE);
    } else {
        fb.fill_rounded_rect(r.x, r.y, r.w, r.h, 4, edge);
        fb.fill_rounded_rect(r.x + 2, r.y + 2, r.w.saturating_sub(4), r.h.saturating_sub(4), 3, Color::SURFACE);
    }
}

/// Slider. `r` is the track area (full control height); `num/den` is the knob
/// position along it.
pub fn slider(fb: &FrameBuffer, r: Rect, num: usize, den: usize, enabled: bool) {
    let cy = r.y + r.h / 2;
    let knob = 16usize.min(r.h);
    let run = r.w.saturating_sub(knob);
    let pos = if den == 0 { 0 } else { run * num.min(den) / den };
    let fill = if enabled { Color::ACCENT } else { Color::TEXT_DIM };
    fb.fill_rounded_rect(r.x, cy.saturating_sub(2), r.w, 4, 2, Color::BORDER);
    fb.fill_rounded_rect(r.x, cy.saturating_sub(2), pos + knob / 2, 4, 2, fill);
    let kx = r.x + pos;
    let ky = cy.saturating_sub(knob / 2);
    fb.fill_rounded_rect(kx, ky, knob, knob, knob / 2, fill);
    fb.fill_rounded_rect(kx + 3, ky + 3, knob.saturating_sub(6), knob.saturating_sub(6), knob / 2, Color::WHITE);
}

/// The box of a text field or dropdown.
pub fn input_box(fb: &FrameBuffer, r: Rect, active: bool, enabled: bool) {
    let bg = if enabled { Color::SURFACE } else { Color::SURFACE_3 };
    let edge = if active { Color::ACCENT } else { Color::TEXT_DIM };
    fb.fill_rounded_rect(r.x, r.y, r.w, r.h, eds::RADIUS_S, edge);
    let t = if active { 2 } else { 1 };
    fb.fill_rounded_rect(r.x + t, r.y + t, r.w.saturating_sub(2 * t), r.h.saturating_sub(2 * t), eds::RADIUS_S - t, bg);
}

/// A down-pointing chevron centered on (cx, cy).
pub fn chevron(fb: &FrameBuffer, cx: usize, cy: usize, c: Color) {
    let (x, y) = (cx as f32, cy as f32);
    fb.aa_seg(x - 4.0, y - 2.0, x, y + 2.0, 0.8, c);
    fb.aa_seg(x, y + 2.0, x + 4.0, y - 2.0, 0.8, c);
}

/// Push button with a centered caption.
pub fn button(fb: &FrameBuffer, r: Rect, caption: &str, primary: bool, enabled: bool, m: &Metrics) {
    let (bg, fg) = match (enabled, primary) {
        (false, _) => (Color::SURFACE_3, Color::TEXT_DIM),
        (true, true) => (Color::ACCENT, Color::WHITE),
        (true, false) => (Color::SURFACE, Color::INK),
    };
    if primary && enabled {
        fb.fill_rounded_rect(r.x, r.y, r.w, r.h, eds::RADIUS_S, bg);
    } else {
        fb.fill_rounded_rect(r.x, r.y, r.w, r.h, eds::RADIUS_S, if enabled { Color::TEXT_DIM } else { Color::BORDER });
        fb.fill_rounded_rect(r.x + 1, r.y + 1, r.w.saturating_sub(2), r.h.saturating_sub(2), eds::RADIUS_S - 1, bg);
    }
    let tw = text::width_px(caption, m.label);
    text::draw_px(fb, r.x + r.w.saturating_sub(tw) / 2, text_y(r.y, r.h, m.label), caption, fg, m.label);
}

/// Usage bar: accent up to 80 %, attention color above, compromised above 92 %.
pub fn meter(fb: &FrameBuffer, r: Rect, pct: u8) {
    let pct = pct.min(100) as usize;
    let c = if pct > 92 {
        eds::SEC_COMPROMISED
    } else if pct > 80 {
        eds::SEC_ATTENTION
    } else {
        eds::SEC_PROTECTED
    };
    fb.fill_rounded_rect(r.x, r.y, r.w, r.h, r.h / 2, Color::BORDER);
    let w = (r.w * pct / 100).max(if pct > 0 { r.h } else { 0 });
    if w > 0 {
        fb.fill_rounded_rect(r.x, r.y, w.min(r.w), r.h, r.h / 2, c);
    }
}

/// Vertical scrollbar: a faint track and the thumb.
pub fn scrollbar(fb: &FrameBuffer, track: Rect, thumb: Rect) {
    fb.fill_rounded_rect(track.x + 3, track.y, track.w.saturating_sub(6), track.h, 3, Color::SURFACE_3);
    fb.fill_rounded_rect(thumb.x + 3, thumb.y, thumb.w.saturating_sub(6), thumb.h, 3, Color::TEXT_DIM);
}
