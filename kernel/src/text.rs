//! Modern text rendering: anti-aliased TTF glyphs via `ab_glyph`.
//!
//! Replaces the 8×8 bitmap font. Two embedded fonts:
//! - **UI** (DM Sans) — proportional, for desktop chrome, labels, buttons.
//! - **MONO** (DejaVu Sans Mono) — for terminal/file listings where columns
//!   must line up.
//!
//! Glyphs are rasterized to coverage values (0..1) and blended over the
//! background with `fb.blend` — hence the soft, non-stepped edges.

use ab_glyph::{Font, FontRef, GlyphId, PxScale, ScaleFont};
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use spin::{Mutex, Once};

use crate::graphics::{Color, FrameBuffer};

/// A rasterized glyph: coverage bitmap + placement relative to the pen.
/// Rasterizing a TTF outline (ab_glyph `outline_glyph` + scan-conversion) is
/// expensive and was redone for every character on every full redraw — the
/// dominant cost of a desktop repaint under TCG. We do it once per
/// (font, char, size) and reuse the coverage; only the (cheap) alpha blend runs
/// per draw. `None` = a glyph with no outline (e.g. space).
struct CachedGlyph {
    w: usize,
    h: usize,
    left: i32,
    top: i32,
    cov: Vec<u8>,
}

static GLYPH_CACHE: Mutex<BTreeMap<(u8, char, u32), Option<CachedGlyph>>> = Mutex::new(BTreeMap::new());

static UI: Once<FontRef<'static>> = Once::new();
static MONO: Once<FontRef<'static>> = Once::new();

fn ui() -> &'static FontRef<'static> {
    UI.call_once(|| {
        FontRef::try_from_slice(include_bytes!("../assets/ui.ttf")).expect("ui.ttf invalid")
    })
}

fn mono() -> &'static FontRef<'static> {
    MONO.call_once(|| {
        FontRef::try_from_slice(include_bytes!("../assets/mono.ttf")).expect("mono.ttf invalid")
    })
}

/// Pixel height for a given legacy `scale` (1/2/3) — UI font.
#[inline]
pub fn ui_px(scale: usize) -> f32 {
    match scale {
        1 => 15.0,
        2 => 25.0,
        3 => 34.0,
        n => 15.0 * n as f32,
    }
}

/// Pixel height for the mono font; chosen so that the advance ≈ 8 px (raster width).
#[inline]
pub fn mono_px(scale: usize) -> f32 {
    13.0 * scale as f32
}

/// Core renderer: draw `s` with `font` at px size `px`. `y` is the top
/// of the text line (baseline = y + ascent), like the old bitmap API.
/// Style flags for styled text: bit0 = bold (synthetic embolden), bit1 = italic
/// (synthetic shear). Works on any glyph without a separate bold/italic font.
pub const STYLE_BOLD: u8 = 1;
pub const STYLE_ITALIC: u8 = 2;

fn render(fb: &FrameBuffer, font: &FontRef, font_id: u8, x: usize, y: usize, s: &str, c: Color, px: f32) {
    render_styled(fb, font, font_id, x, y, s, c, px, 0)
}

fn render_styled(fb: &FrameBuffer, font: &FontRef, font_id: u8, x: usize, y: usize, s: &str, c: Color, px: f32, style: u8) {
    let scale = PxScale::from(px);
    let sf = font.as_scaled(scale);
    let baseline = (y as f32 + sf.ascent() + 0.5) as i32;
    let px_key = px.to_bits();
    let mut caret = x as f32;
    let mut prev: Option<GlyphId> = None;
    let mut cache = GLYPH_CACHE.lock();
    for ch in s.chars() {
        let gid = font.glyph_id(ch);
        if let Some(p) = prev {
            caret += sf.kern(p, gid);
        }
        let entry = cache.entry((font_id, ch, px_key)).or_insert_with(|| {
            // Rasterize the glyph once, with the pen at the origin, so the stored
            // coverage + (left,top) offsets are pen-relative and reusable.
            let g = gid.with_scale_and_position(scale, ab_glyph::point(0.0, 0.0));
            font.outline_glyph(g).map(|outlined| {
                let bb = outlined.px_bounds();
                let left = bb.min.x as i32;
                let top = bb.min.y as i32;
                // +2 px of slack so the rasterizer's coverage grid always fits
                // (no floor/ceil in no_std; `as` truncates toward zero).
                let gw = (bb.max.x - bb.min.x) as usize + 2;
                let gh = (bb.max.y - bb.min.y) as usize + 2;
                let mut cov = alloc::vec![0u8; gw * gh];
                outlined.draw(|gx, gy, v| {
                    let (ix, iy) = (gx as usize, gy as usize);
                    if ix < gw && iy < gh {
                        cov[iy * gw + ix] = (v * 255.0) as u8;
                    }
                });
                CachedGlyph { w: gw, h: gh, left, top, cov }
            })
        });
        if let Some(g) = entry {
            let base_x = (caret + 0.5) as i32 + g.left;
            let base_y = baseline + g.top;
            for gy in 0..g.h {
                let sy = base_y + gy as i32;
                if sy < 0 {
                    continue;
                }
                let row = gy * g.w;
                // Italic: shear x by the height above the baseline (~0.22 slant).
                let shear = if style & STYLE_ITALIC != 0 {
                    ((g.h as i32 - gy as i32) * 22) / 100
                } else {
                    0
                };
                for gx in 0..g.w {
                    let a = g.cov[row + gx];
                    if a > 0 {
                        let sx = base_x + gx as i32 + shear;
                        if sx >= 0 {
                            fb.blend(sx as usize, sy as usize, c, a);
                            // Bold: a second pass one pixel right thickens the stem.
                            if style & STYLE_BOLD != 0 {
                                fb.blend(sx as usize + 1, sy as usize, c, a);
                            }
                        }
                    }
                }
            }
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

// ── Public API (proportional, UI font) ──────────────────────────────────────
pub fn draw_string(fb: &FrameBuffer, x: usize, y: usize, s: &str, c: Color, scale: usize) {
    render(fb, ui(), 0, x, y, s, c, ui_px(scale));
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
    render(fb, ui(), 0, x, y, s, c, ui_px(scale));
}

// ── Exact px sizes (for design-faithful cards: clock 44px etc.) ─────────────
pub fn draw_px(fb: &FrameBuffer, x: usize, y: usize, s: &str, c: Color, px: f32) {
    render(fb, ui(), 0, x, y, s, c, px);
}

/// Draw with bold/italic style flags (see STYLE_BOLD / STYLE_ITALIC) and a font
/// family: 0 = UI sans (Inter), 1 = monospace (DejaVu). Bold adds ~1px per glyph.
pub fn draw_px_styled(fb: &FrameBuffer, x: usize, y: usize, s: &str, c: Color, px: f32, style: u8, family: u8) {
    let f = if family == 1 { mono() } else { ui() };
    render_styled(fb, f, family, x, y, s, c, px, style);
}

/// Width of styled text (bold widens each glyph by ~1px; family selects the font).
pub fn width_px_styled(s: &str, px: f32, style: u8, family: u8) -> usize {
    let f = if family == 1 { mono() } else { ui() };
    let base = measure(f, s, px);
    if style & STYLE_BOLD != 0 {
        base + s.chars().count()
    } else {
        base
    }
}

pub fn width_px(s: &str, px: f32) -> usize {
    measure(ui(), s, px)
}

/// Height (ascent+descent) of the UI font at px size — for vertical centering.
pub fn line_height(px: f32) -> usize {
    let sf = ui().as_scaled(PxScale::from(px));
    (sf.ascent() - sf.descent()) as usize
}

// ── Monospace (terminal/code) ──────────────────────────────────────────────
pub fn draw_mono(fb: &FrameBuffer, x: usize, y: usize, s: &str, c: Color, scale: usize) {
    render(fb, mono(), 1, x, y, s, c, mono_px(scale));
}

pub fn mono_width(s: &str, scale: usize) -> usize {
    measure(mono(), s, mono_px(scale))
}

/// Advance of a single mono character (for cursor/column calculation).
pub fn mono_advance(scale: usize) -> usize {
    let f = mono();
    let sf = f.as_scaled(PxScale::from(mono_px(scale)));
    sf.h_advance(f.glyph_id('M')) as usize
}
