//! EuroReken — een ECHTE interactieve rekenmachine (geen mockup).
//!
//! Het venster toont de LIVE toestand: de expressie die de gebruiker met
//! toetsenbord of muis invoert, en het ECHTE resultaat dat de [`euroreken`]-engine
//! per wijziging berekent. De toestand zit in `win.content` (`[expr, result]`);
//! de desktop-loop muteert die op toetsaanslag/muisklik. Niets is hardcoded.

use crate::graphics::{Color, FrameBuffer};
use crate::text;

const TITLEBAR_H: usize = 44;

/// De 20 knoppen (4 kolommen × 5 rijen). `.1` is het teken dat de invoer krijgt;
/// `'C'` = wissen, `0x08` = backspace, `'='` = evalueren (resultaat is al live).
pub const BUTTONS: [(&str, char); 20] = [
    ("C", 'C'), ("(", '('), (")", ')'), ("\u{232B}", '\u{8}'),
    ("7", '7'), ("8", '8'), ("9", '9'), ("/", '/'),
    ("4", '4'), ("5", '5'), ("6", '6'), ("*", '*'),
    ("1", '1'), ("2", '2'), ("3", '3'), ("-", '-'),
    ("0", '0'), (".", '.'), ("=", '='), ("+", '+'),
];

const COLS: usize = 4;
const ROWS: usize = 5;

/// Het rechthoekje (x,y,w,h) van knop `i`, binnen het knoppen-gebied.
fn button_rect(i: usize, ax: usize, ay: usize, aw: usize, ah: usize) -> (usize, usize, usize, usize) {
    let gap = 8usize;
    let cw = (aw - gap * (COLS - 1)) / COLS;
    let chh = (ah - gap * (ROWS - 1)) / ROWS;
    let col = i % COLS;
    let row = i / COLS;
    (ax + col * (cw + gap), ay + row * (chh + gap), cw, chh)
}

/// Het knoppen-gebied binnen het venster-lichaam (onder het display).
fn button_area(x: usize, y: usize, w: usize, h: usize) -> (usize, usize, usize, usize) {
    let pad = 16usize;
    let disp_h = 92usize;
    (x + pad, y + disp_h + pad, w.saturating_sub(pad * 2), h.saturating_sub(disp_h + pad * 2))
}

/// Welke knop-teken ligt onder (mx,my)? `win_*` = volledige venstergeometrie.
pub fn button_at(win_x: usize, win_y: usize, win_w: usize, win_h: usize, mx: usize, my: usize) -> Option<char> {
    let (bx, by, bw, bh) = (win_x, win_y + TITLEBAR_H, win_w, win_h.saturating_sub(TITLEBAR_H));
    let (ax, ay, aw, ah) = button_area(bx, by, bw, bh);
    for i in 0..BUTTONS.len() {
        let (rx, ry, rw, rh) = button_rect(i, ax, ay, aw, ah);
        if mx >= rx && mx < rx + rw && my >= ry && my < ry + rh {
            return Some(BUTTONS[i].1);
        }
    }
    None
}

/// Render het rekenmachine-lichaam. `content[0]` = expressie, `content[1]` = resultaat.
pub fn render(fb: &FrameBuffer, win_x: usize, win_y: usize, win_w: usize, win_h: usize, content: &[alloc::string::String]) {
    let x = win_x;
    let y = win_y + TITLEBAR_H;
    let w = win_w;
    let h = win_h.saturating_sub(TITLEBAR_H);

    // Achtergrond.
    fb.fill_rect(x, y, w, h, Color::SURFACE);

    // ── Display (toont de ECHTE invoer + het ECHTE resultaat) ──
    let pad = 16usize;
    let disp_h = 92usize;
    fb.fill_rounded_rect(x + pad, y + pad, w - pad * 2, disp_h - 8, crate::eds::RADIUS_M, Color::rgb(0x20, 0x2A, 0x36));
    let empty = alloc::string::String::new();
    let expr = content.first().unwrap_or(&empty);
    let result = content.get(1).unwrap_or(&empty);
    // Expressie (klein, lichtgrijs, rechts).
    let ew = text::width_px(expr, 16.0);
    let exr = (x + w - pad - 14).saturating_sub(ew);
    text::draw_px(fb, exr.max(x + pad + 14), y + pad + 14, expr, Color::rgb(0x9A, 0xA6, 0xB4), 16.0);
    // Resultaat (groot, wit, rechts).
    let disp_res = if result.is_empty() { "0" } else { result.as_str() };
    let rw = text::width_px(disp_res, 34.0);
    let rxr = (x + w - pad - 14).saturating_sub(rw);
    text::draw_px(fb, rxr.max(x + pad + 14), y + pad + 40, disp_res, Color::WHITE, 34.0);

    // ── Knoppen ──
    let (ax, ay, aw, ah) = button_area(x, y, w, h);
    for (i, (label, ch)) in BUTTONS.iter().enumerate() {
        let (rx, ry, rw_, rh) = button_rect(i, ax, ay, aw, ah);
        // Kleurcodering: operatoren = accent, '=' = gevuld accent, C = rood-zacht, cijfers = kaart.
        let (bg, fg) = match ch {
            '=' => (Color::ACCENT, Color::WHITE),
            '+' | '-' | '*' | '/' | '(' | ')' => (Color::ACCENT_SOFT, Color::ACCENT),
            'C' => (Color::rgb(0xFD, 0xEA, 0xE8), Color::rgb(0xD6, 0x45, 0x3D)),
            '\u{8}' => (Color::SURFACE_3, Color::TEXT_SEC),
            _ => (Color::CARD, Color::INK),
        };
        fb.fill_rounded_rect(rx, ry, rw_, rh, crate::eds::RADIUS_M, bg);
        if matches!(ch, '0'..='9' | '.') {
            fb.draw_border(rx, ry, rw_, rh, 1, Color::BORDER);
        }
        let lw = text::width_px(label, 19.0);
        text::draw_px(fb, rx + (rw_ - lw) / 2, ry + (rh.saturating_sub(19)) / 2, label, fg, 19.0);
    }
}

/// Verwerk één invoer-teken op de rekenmachine-toestand. Past `content` aan en
/// herberekent het ECHTE resultaat via de euroreken-engine. Retourneert true als
/// er iets veranderde (zodat de loop kan hertekenen).
pub fn input(content: &mut alloc::vec::Vec<alloc::string::String>, ch: char) -> bool {
    while content.len() < 2 {
        content.push(alloc::string::String::new());
    }
    match ch {
        'C' => content[0].clear(),
        '\u{8}' => {
            content[0].pop();
        }
        '=' => {} // resultaat is al live; '=' bevestigt enkel
        c => content[0].push(c),
    }
    // ECHTE evaluatie via de euroreken-engine.
    let expr = content[0].clone();
    content[1] = if expr.trim().is_empty() {
        alloc::string::String::from("0")
    } else {
        match euroreken::eval(&expr) {
            Ok(v) => fmt_num(v),
            Err(_) => alloc::string::String::from("…"),
        }
    };
    true
}

/// Format een f64 netjes (gehele getallen zonder decimalen).
fn fmt_num(v: f64) -> alloc::string::String {
    if v.is_nan() {
        return alloc::string::String::from("fout");
    }
    if v == (v as i64) as f64 && euroreken::math::fabs(v) < 1e15 {
        alloc::format!("{}", v as i64)
    } else {
        // Beperk tot ~10 significante cijfers.
        let s = alloc::format!("{:.6}", v);
        let trimmed = s.trim_end_matches('0').trim_end_matches('.');
        alloc::string::String::from(trimmed)
    }
}
