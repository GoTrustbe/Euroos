//! EuroReken — a REAL interactive calculator (not a mockup).
//!
//! The window shows the LIVE state: the expression the user enters with
//! keyboard or mouse, and the REAL result that the [`euroreken`] engine
//! computes on each change. The state lives in `win.content` (`[expr, result]`);
//! the desktop loop mutates it on keystroke/mouse-click. Nothing is hardcoded.

use crate::graphics::{Color, FrameBuffer};
use crate::text;

const TITLEBAR_H: usize = 44;

/// The 20 buttons (4 columns × 5 rows). `.1` is the character the input receives;
/// `'C'` = clear, `0x08` = backspace, `'='` = evaluate (result is already live).
pub const BUTTONS: [(&str, char); 20] = [
    ("C", 'C'), ("(", '('), (")", ')'), ("\u{232B}", '\u{8}'),
    ("7", '7'), ("8", '8'), ("9", '9'), ("/", '/'),
    ("4", '4'), ("5", '5'), ("6", '6'), ("*", '*'),
    ("1", '1'), ("2", '2'), ("3", '3'), ("-", '-'),
    ("0", '0'), (".", '.'), ("=", '='), ("+", '+'),
];

const COLS: usize = 4;
const ROWS: usize = 5;

/// The little rectangle (x,y,w,h) of button `i`, within the buttons area.
fn button_rect(i: usize, ax: usize, ay: usize, aw: usize, ah: usize) -> (usize, usize, usize, usize) {
    let gap = 8usize;
    let cw = (aw - gap * (COLS - 1)) / COLS;
    let chh = (ah - gap * (ROWS - 1)) / ROWS;
    let col = i % COLS;
    let row = i / COLS;
    (ax + col * (cw + gap), ay + row * (chh + gap), cw, chh)
}

/// The buttons area within the window body (below the display).
fn button_area(x: usize, y: usize, w: usize, h: usize) -> (usize, usize, usize, usize) {
    let pad = 16usize;
    let disp_h = 92usize;
    (x + pad, y + disp_h + pad, w.saturating_sub(pad * 2), h.saturating_sub(disp_h + pad * 2))
}

/// Which button-character lies under (mx,my)? `win_*` = full window geometry.
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

/// Render the calculator body. `content[0]` = expression, `content[1]` = result.
pub fn render(fb: &FrameBuffer, win_x: usize, win_y: usize, win_w: usize, win_h: usize, content: &[alloc::string::String]) {
    let x = win_x;
    let y = win_y + TITLEBAR_H;
    let w = win_w;
    let h = win_h.saturating_sub(TITLEBAR_H);

    // Background.
    fb.fill_rect(x, y, w, h, Color::SURFACE);

    // ── Display (shows the REAL input + the REAL result) ──
    let pad = 16usize;
    let disp_h = 92usize;
    fb.fill_rounded_rect(x + pad, y + pad, w - pad * 2, disp_h - 8, crate::eds::RADIUS_M, Color::rgb(0x20, 0x2A, 0x36));
    let empty = alloc::string::String::new();
    let expr = content.first().unwrap_or(&empty);
    let result = content.get(1).unwrap_or(&empty);
    // Expression (small, light gray, right-aligned).
    let ew = text::width_px(expr, 16.0);
    let exr = (x + w - pad - 14).saturating_sub(ew);
    text::draw_px(fb, exr.max(x + pad + 14), y + pad + 14, expr, Color::rgb(0x9A, 0xA6, 0xB4), 16.0);
    // Result (large, white, right-aligned).
    let disp_res = if result.is_empty() { "0" } else { result.as_str() };
    let rw = text::width_px(disp_res, 34.0);
    let rxr = (x + w - pad - 14).saturating_sub(rw);
    text::draw_px(fb, rxr.max(x + pad + 14), y + pad + 40, disp_res, Color::WHITE, 34.0);

    // ── Buttons ──
    let (ax, ay, aw, ah) = button_area(x, y, w, h);
    for (i, (label, ch)) in BUTTONS.iter().enumerate() {
        let (rx, ry, rw_, rh) = button_rect(i, ax, ay, aw, ah);
        // Color coding: operators = accent, '=' = filled accent, C = soft-red, digits = card.
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

/// Process one input character against the calculator state. Adjusts `content` and
/// recomputes the REAL result via the euroreken engine. Returns true if
/// something changed (so the loop can redraw).
pub fn input(content: &mut alloc::vec::Vec<alloc::string::String>, ch: char) -> bool {
    while content.len() < 2 {
        content.push(alloc::string::String::new());
    }
    match ch {
        'C' => content[0].clear(),
        '\u{8}' => {
            content[0].pop();
        }
        '=' => {} // result is already live; '=' only confirms
        c => content[0].push(c),
    }
    // REAL evaluation via the euroreken engine.
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

/// Format an f64 neatly (whole numbers without decimals).
fn fmt_num(v: f64) -> alloc::string::String {
    if v.is_nan() {
        return alloc::string::String::from("error");
    }
    if v == (v as i64) as f64 && euroreken::math::fabs(v) < 1e15 {
        alloc::format!("{}", v as i64)
    } else {
        // Limit to ~10 significant digits.
        let s = alloc::format!("{:.6}", v);
        let trimmed = s.trim_end_matches('0').trim_end_matches('.');
        alloc::string::String::from(trimmed)
    }
}
