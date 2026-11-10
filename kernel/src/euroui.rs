//! EuroUI — the widget toolkit on top of the compositor (Track 5).
//!
//! Two layers, both drawn with the EDS tokens (spacing on the `eu` unit, the
//! radius system, the security color language) and `text::draw_px`:
//!
//! - [`form`] — interactive forms: a scrollable column of labelled controls
//!   (toggle, checkbox, dropdown, slider, text field, button, list) with
//!   hit-testing, Tab focus and events. Settings screens are built from this;
//!   start there for anything a user can change. [`draw`] holds the control
//!   painters it uses.
//! - [`Widget`] + [`draw_panel`] — the original static stack (heading, row,
//!   badge, …) for read-only panels.
//!
//! The text scale ([`text_scale`]) applies to forms: one user setting grows the
//! text and the controls of every panel built on them.

use alloc::string::String;
use core::sync::atomic::{AtomicU32, Ordering};

use crate::eds;
use crate::graphics::{Color, FrameBuffer};
use crate::text;

#[path = "uikit_draw.rs"]
pub mod draw;
#[path = "uikit_form.rs"]
pub mod form;

/// Smallest / largest / step of the text scale, in percent.
pub const TEXT_SCALE_MIN: u32 = 90;
pub const TEXT_SCALE_MAX: u32 = 130;
pub const TEXT_SCALE_STEP: u32 = 10;

static TEXT_SCALE: AtomicU32 = AtomicU32::new(100);

/// The text scale of form-based panels, in percent (100 = default).
pub fn text_scale() -> u32 {
    TEXT_SCALE.load(Ordering::Relaxed)
}

/// Set the text scale; out-of-range values are clamped.
pub fn set_text_scale(pct: u32) {
    TEXT_SCALE.store(pct.clamp(TEXT_SCALE_MIN, TEXT_SCALE_MAX), Ordering::Relaxed);
}

/// A static UI element. Apps assemble a `Vec<Widget>` (vertical stack).
#[derive(Clone)]
pub enum Widget {
    /// Section header (large).
    Heading(String),
    /// Caption / help text (dimmed).
    Caption(String),
    /// Label row with a value right-aligned (settings style).
    Row(String, String),
    /// Label + on/off toggle (green = on). Display only; use `form` for a
    /// switch the user can operate.
    Toggle(String, bool),
    /// Button; `primary` = filled accent button, otherwise subtle.
    Button(String, bool),
    /// Pill badge with the security color language.
    Badge(String, Color),
    /// Thin separator line.
    Divider,
    /// Vertical space (in `eu` units).
    Spacer(usize),
}

/// Draw a vertical stack of widgets within the area (`x`,`y`,`w`).
pub fn draw_panel(fb: &FrameBuffer, x: usize, y: usize, w: usize, widgets: &[Widget]) {
    const BODY: f32 = 13.5;
    const HEAD: f32 = 20.0;
    let pad = eds::eu(4);
    let cx = x + pad;
    let inner = w.saturating_sub(pad * 2);
    let mut cy = y + pad;
    let gap = eds::eu(2);
    let rowh = eds::eu(8);

    for wdg in widgets {
        match wdg {
            Widget::Heading(t) => {
                text::draw_px(fb, cx, cy, t, Color::INK, HEAD);
                cy += text::line_height(HEAD) + eds::eu(2);
            }
            Widget::Caption(t) => {
                text::draw_px(fb, cx, cy, t, Color::TEXT_SEC, BODY);
                cy += text::line_height(BODY) + eds::eu(2);
            }
            Widget::Row(l, val) => {
                fb.fill_rounded_rect(cx, cy, inner, rowh, eds::RADIUS_S, Color::CARD);
                let ty = draw::text_y(cy, rowh, BODY);
                text::draw_px(fb, cx + eds::eu(3), ty, l, Color::TEXT_SEC, BODY);
                let vw = text::width_px(val, BODY);
                text::draw_px(fb, cx + inner.saturating_sub(vw + eds::eu(3)), ty, val, Color::INK, BODY);
                cy += rowh + gap;
            }
            Widget::Toggle(l, on) => {
                fb.fill_rounded_rect(cx, cy, inner, rowh, eds::RADIUS_S, Color::CARD);
                text::draw_px(fb, cx + eds::eu(3), draw::text_y(cy, rowh, BODY), l, Color::TEXT_SEC, BODY);
                let (pw, ph) = (eds::eu(10), eds::eu(5) + 2);
                let track = form::Rect::new(cx + inner.saturating_sub(pw + eds::eu(3)), cy + (rowh - ph) / 2, pw, ph);
                draw::toggle(fb, track, *on, true);
                cy += rowh + gap;
            }
            Widget::Button(t, primary) => {
                let bh = eds::eu(9);
                draw::button(fb, form::Rect::new(cx, cy, inner, bh), t, *primary, true, &form::metrics());
                cy += bh + gap;
            }
            Widget::Badge(t, c) => {
                let bw = text::width_px(t, BODY) + eds::eu(4);
                let bh = eds::eu(6);
                fb.fill_rounded_rect(cx, cy, bw, bh, bh / 2, Color::SURFACE);
                text::draw_px(fb, cx + eds::eu(2), draw::text_y(cy, bh, BODY), t, *c, BODY);
                cy += bh + gap;
            }
            Widget::Divider => {
                fb.fill_rect(cx, cy + eds::eu(1), inner, 1, Color::BORDER);
                cy += eds::eu(3);
            }
            Widget::Spacer(n) => {
                cy += eds::eu(*n);
            }
        }
    }
}
