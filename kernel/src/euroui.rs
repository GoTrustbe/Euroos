//! EuroUI — a widget + layout layer on top of the compositor (Track 5).
//!
//! Apps build a screen out of REUSABLE widgets (heading, row, toggle, button,
//! badge, …) in a vertical stack; EuroUI lays them out and draws them with the
//! EDS tokens (spacing on the `eu` unit, the radius system, the security
//! color language) — never with arbitrary values. This way all apps look consistent
//! and no longer have to draw pixels by hand.

use alloc::string::String;

use crate::eds;
use crate::font::{draw_string, draw_string_centered, text_width, CHAR_HEIGHT};
use crate::graphics::{Color, FrameBuffer};

/// A UI element. Apps assemble a `Vec<Widget>` (vertical stack).
#[derive(Clone)]
pub enum Widget {
    /// Section header (large).
    Heading(String),
    /// Caption / help text (dimmed).
    Caption(String),
    /// Label row with a value right-aligned (settings style).
    Row(String, String),
    /// Label + on/off toggle (green = on).
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
    let pad = eds::eu(4);
    let cx = x + pad;
    let inner = w.saturating_sub(pad * 2);
    let mut cy = y + pad;
    let gap = eds::eu(2);
    let rowh = eds::eu(8);

    for wdg in widgets {
        match wdg {
            Widget::Heading(t) => {
                draw_string(fb, cx, cy, t, Color::INK, 2);
                cy += CHAR_HEIGHT * 2 + eds::eu(2);
            }
            Widget::Caption(t) => {
                draw_string(fb, cx, cy, t, Color::TEXT_SEC, 1);
                cy += CHAR_HEIGHT + eds::eu(2);
            }
            Widget::Row(l, val) => {
                fb.fill_rounded_rect(cx, cy, inner, rowh, eds::RADIUS_S, Color::CARD);
                let ty = cy + (rowh - CHAR_HEIGHT) / 2;
                draw_string(fb, cx + eds::eu(3), ty, l, Color::TEXT_SEC, 1);
                let vw = text_width(val, 1);
                draw_string(fb, cx + inner - vw - eds::eu(3), ty, val, Color::INK, 1);
                cy += rowh + gap;
            }
            Widget::Toggle(l, on) => {
                fb.fill_rounded_rect(cx, cy, inner, rowh, eds::RADIUS_S, Color::CARD);
                let ty = cy + (rowh - CHAR_HEIGHT) / 2;
                draw_string(fb, cx + eds::eu(3), ty, l, Color::TEXT_SEC, 1);
                // Toggle on the right.
                let pw = eds::eu(9);
                let ph = eds::eu(4);
                let px = cx + inner - pw - eds::eu(3);
                let py = cy + (rowh - ph) / 2;
                let track = if *on { eds::SEC_VERIFIED } else { Color::BORDER };
                fb.fill_rounded_rect(px, py, pw, ph, ph / 2, track);
                let knob = ph.saturating_sub(4);
                let kx = if *on { px + pw - knob - 2 } else { px + 2 };
                fb.fill_rounded_rect(kx, py + 2, knob, knob, knob / 2, Color::WHITE);
                cy += rowh + gap;
            }
            Widget::Button(t, primary) => {
                let bh = eds::eu(9);
                let (bg, fg) = if *primary {
                    (Color::ACCENT, Color::WHITE)
                } else {
                    (Color::SURFACE, Color::INK)
                };
                fb.fill_rounded_rect(cx, cy, inner, bh, eds::RADIUS_M, bg);
                if !*primary {
                    fb.draw_border(cx, cy, inner, bh, 1, Color::BORDER);
                }
                draw_string_centered(fb, cx, inner, cy + (bh - CHAR_HEIGHT) / 2, t, fg, 1);
                cy += bh + gap;
            }
            Widget::Badge(t, c) => {
                let bw = text_width(t, 1) + eds::eu(4);
                let bh = eds::eu(5);
                fb.fill_rounded_rect(cx, cy, bw, bh, bh / 2, Color::SURFACE);
                draw_string(fb, cx + eds::eu(2), cy + (bh - CHAR_HEIGHT) / 2, t, *c, 1);
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
