//! EuroOS app-icon-systeem — kleurrijke squircle-tegels (uit `appicons.js`).
//!
//! Elke app heeft een afgeronde tegel met een 150°-verloop, een witte euicon-
//! glyph (~52% van de tegel) en een zachte, getinte slagschaduw — precies zoals
//! het EDS de app-iconen tekent (zie `assets/appicons.js`, `v3-dock.png`).

use crate::graphics::{Color, FrameBuffer};

/// (verloop-van, verloop-naar, tint/schaduw, glyph-naam in `icons`).
struct Def {
    g0: Color,
    g1: Color,
    tint: Color,
    glyph: &'static str,
}

fn def(id: &str) -> Def {
    let c = Color::rgb;
    match id {
        "files" => Def { g0: c(0x4C, 0x90, 0xF0), g1: c(0x20, 0x59, 0xC8), tint: c(0x20, 0x59, 0xC8), glyph: "files" },
        "browser" => Def { g0: c(0x34, 0xB6, 0xC9), g1: c(0x1E, 0x7E, 0x96), tint: c(0x1E, 0x7E, 0x96), glyph: "browser" },
        "mail" => Def { g0: c(0xF0, 0x8A, 0x5D), g1: c(0xD4, 0x5A, 0x3C), tint: c(0xD4, 0x5A, 0x3C), glyph: "mail" },
        "settings" => Def { g0: c(0x7C, 0x8A, 0xA0), g1: c(0x4B, 0x57, 0x6B), tint: c(0x4B, 0x57, 0x6B), glyph: "settings" },
        "store" => Def { g0: c(0xF0, 0xBE, 0x4A), g1: c(0xD6, 0x96, 0x2A), tint: c(0xD6, 0x96, 0x2A), glyph: "store" },
        "photos" => Def { g0: c(0x9A, 0x7B, 0xEA), g1: c(0x6A, 0x4B, 0xD0), tint: c(0x6A, 0x4B, 0xD0), glyph: "photos" },
        "terminal" => Def { g0: c(0x3A, 0x4A, 0x5E), g1: c(0x1C, 0x27, 0x35), tint: c(0x1C, 0x27, 0x35), glyph: "terminal" },
        "vault" => Def { g0: c(0x2E, 0xA8, 0x6A), g1: c(0x14, 0x7A, 0x4A), tint: c(0x14, 0x7A, 0x4A), glyph: "shieldCheck" },
        // Nieuwe dock-apps (AG-1): notities (amber), klok (violet), agent (indigo).
        "notes" => Def { g0: c(0xF6, 0xC8, 0x5A), g1: c(0xE2, 0xA3, 0x3A), tint: c(0xE2, 0xA3, 0x3A), glyph: "doc" },
        "clock" => Def { g0: c(0x9A, 0x7B, 0xEA), g1: c(0x6A, 0x4B, 0xD0), tint: c(0x6A, 0x4B, 0xD0), glyph: "clock" },
        "star" => Def { g0: c(0x6E, 0x8B, 0xF5), g1: c(0x3B, 0x4E, 0xC8), tint: c(0x3B, 0x4E, 0xC8), glyph: "star" },
        "text" => Def { g0: c(0x5E, 0x9C, 0xE0), g1: c(0x2B, 0x6C, 0xB0), tint: c(0x2B, 0x6C, 0xB0), glyph: "doc" },
        "monitor" => Def { g0: c(0x46, 0xC8, 0x90), g1: c(0x1F, 0x9D, 0x6B), tint: c(0x1F, 0x9D, 0x6B), glyph: "grid" },
        "log" => Def { g0: c(0xE8, 0x8A, 0x6A), g1: c(0xB0, 0x4A, 0x2B), tint: c(0xB0, 0x4A, 0x2B), glyph: "shieldCheck" },
        _ => Def { g0: c(0x4C, 0x90, 0xF0), g1: c(0x20, 0x59, 0xC8), tint: c(0x20, 0x59, 0xC8), glyph: "files" },
    }
}

/// Teken een app-tegel met linkerbovenhoek (x,y) en zijde `size`.
pub fn draw_tile(fb: &FrameBuffer, x: usize, y: usize, size: usize, id: &str) {
    let d = def(id);
    // Getinte slagschaduw onder de tegel (geeft de "zwevende" look).
    let spread = (size as i32 * 18 / 100).max(5);
    let off = (size as i32 * 9 / 100).max(3);
    fb.drop_shadow(x, y, size, size, spread, off, d.tint);
    // Verlopen squircle (radius 28% — het EDS-icoonprofiel).
    let r = size * 28 / 100;
    fb.fill_rounded_rect_grad(x, y, size, size, r, d.g0, d.g1);
    // Subtiele inset-highlight bovenaan (glas-look).
    let hl = (size / 12).max(2);
    fb.fill_rounded_rect(x + r / 2, y + hl / 2, size - r, hl, hl / 2, Color::WHITE.over(d.g0, 70));
    // Witte glyph, gecentreerd op ~52%.
    let gs = size * 52 / 100;
    let gx = x + (size - gs) / 2;
    let gy = y + (size - gs) / 2;
    crate::icons::draw(fb, d.glyph, gx, gy, gs, Color::WHITE);
}
