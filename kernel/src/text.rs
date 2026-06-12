//! Moderne tekst-rendering: anti-aliased TTF-glyphs via `ab_glyph`.
//!
//! Vervangt de 8×8 bitmapfont. Twee ingebedde fonts:
//! - **UI** (DM Sans) — proportioneel, voor desktop-chrome, labels, knoppen.
//! - **MONO** (DejaVu Sans Mono) — voor terminal/bestandslijsten waar kolommen
//!   moeten uitlijnen.
//!
//! Glyphs worden gerasterd naar dekkings-waarden (0..1) en met `fb.blend`
//! over de achtergrond gemengd — vandaar de zachte, niet-getrapte randen.

use ab_glyph::{Font, FontRef, GlyphId, PxScale, ScaleFont};
use spin::Once;

use crate::graphics::{Color, FrameBuffer};

static UI: Once<FontRef<'static>> = Once::new();
static MONO: Once<FontRef<'static>> = Once::new();

fn ui() -> &'static FontRef<'static> {
    UI.call_once(|| {
        FontRef::try_from_slice(include_bytes!("../assets/ui.ttf")).expect("ui.ttf ongeldig")
    })
}

fn mono() -> &'static FontRef<'static> {
    MONO.call_once(|| {
        FontRef::try_from_slice(include_bytes!("../assets/mono.ttf")).expect("mono.ttf ongeldig")
    })
}

/// Pixelhoogte voor een gegeven legacy-`scale` (1/2/3) — UI-font.
#[inline]
pub fn ui_px(scale: usize) -> f32 {
    match scale {
        1 => 15.0,
        2 => 25.0,
        3 => 34.0,
        n => 15.0 * n as f32,
    }
}

/// Pixelhoogte voor de mono-font; gekozen zodat de advance ≈ 8 px (rasterbreedte).
#[inline]
pub fn mono_px(scale: usize) -> f32 {
    13.0 * scale as f32
}

/// Kernrenderer: teken `s` met `font` op px-grootte `px`. `y` is de bovenkant
/// van de tekstregel (baseline = y + ascent), zoals de oude bitmap-API.
fn render(fb: &FrameBuffer, font: &FontRef, x: usize, y: usize, s: &str, c: Color, px: f32) {
    let scale = PxScale::from(px);
    let sf = font.as_scaled(scale);
    let baseline = y as f32 + sf.ascent();
    let mut caret = x as f32;
    let mut prev: Option<GlyphId> = None;
    for ch in s.chars() {
        let gid = font.glyph_id(ch);
        if let Some(p) = prev {
            caret += sf.kern(p, gid);
        }
        let glyph = gid.with_scale_and_position(scale, ab_glyph::point(caret, baseline));
        if let Some(outlined) = font.outline_glyph(glyph) {
            let bb = outlined.px_bounds();
            let ox = bb.min.x as i32;
            let oy = bb.min.y as i32;
            outlined.draw(|gx, gy, cov| {
                let px_ = ox + gx as i32;
                let py_ = oy + gy as i32;
                if px_ >= 0 && py_ >= 0 && cov > 0.0 {
                    fb.blend(px_ as usize, py_ as usize, c, (cov * 255.0) as u8);
                }
            });
        }
        caret += sf.h_advance(gid);
        prev = Some(gid);
    }
}

fn measure(font: &FontRef, s: &str, px: f32) -> usize {
    let sf = font.as_scaled(PxScale::from(px));
    let mut w = 0.0f32;
    let mut prev: Option<GlyphId> = None;
    for ch in s.chars() {
        let gid = font.glyph_id(ch);
        if let Some(p) = prev {
            w += sf.kern(p, gid);
        }
        w += sf.h_advance(gid);
        prev = Some(gid);
    }
    w as usize
}

// ── Publieke API (proportioneel, UI-font) ──────────────────────────────────
pub fn draw_string(fb: &FrameBuffer, x: usize, y: usize, s: &str, c: Color, scale: usize) {
    render(fb, ui(), x, y, s, c, ui_px(scale));
}

pub fn text_width(s: &str, scale: usize) -> usize {
    measure(ui(), s, ui_px(scale))
}

pub fn draw_string_centered(
    fb: &FrameBuffer,
    zone_x: usize,
    zone_w: usize,
    y: usize,
    s: &str,
    c: Color,
    scale: usize,
) {
    let w = text_width(s, scale);
    let x = if w < zone_w { zone_x + (zone_w - w) / 2 } else { zone_x };
    render(fb, ui(), x, y, s, c, ui_px(scale));
}

// ── Exacte px-groottes (voor design-getrouwe kaarten: klok 44px enz.) ───────
pub fn draw_px(fb: &FrameBuffer, x: usize, y: usize, s: &str, c: Color, px: f32) {
    render(fb, ui(), x, y, s, c, px);
}

pub fn width_px(s: &str, px: f32) -> usize {
    measure(ui(), s, px)
}

/// Hoogte (ascent+descent) van de UI-font op px-grootte — voor verticaal centreren.
pub fn line_height(px: f32) -> usize {
    let sf = ui().as_scaled(PxScale::from(px));
    (sf.ascent() - sf.descent()) as usize
}

// ── Monospace (terminal/code) ──────────────────────────────────────────────
pub fn draw_mono(fb: &FrameBuffer, x: usize, y: usize, s: &str, c: Color, scale: usize) {
    render(fb, mono(), x, y, s, c, mono_px(scale));
}

pub fn mono_width(s: &str, scale: usize) -> usize {
    measure(mono(), s, mono_px(scale))
}

/// Advance van één mono-teken (voor cursor-/kolomberekening).
pub fn mono_advance(scale: usize) -> usize {
    let f = mono();
    let sf = f.as_scaled(PxScale::from(mono_px(scale)));
    sf.h_advance(f.glyph_id('M')) as usize
}
