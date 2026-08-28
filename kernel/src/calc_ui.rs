//! EuroReken — a REAL interactive calculator (not a mockup).
//!
//! The window shows the LIVE state: the expression the user enters with
//! keyboard or mouse, and the REAL result that the [`euroreken`] engine
//! computes on each change. The state lives in `win.content` (`[expr, result]`);
//! the desktop loop mutates it on keystroke/mouse-click. Nothing is hardcoded.

use crate::graphics::{Color, FrameBuffer};
use crate::text;

const TITLEBAR_H: usize = 44;

/// The full keypad (6 columns × 6 rows): scientific functions, memory keys and
/// the classic grid. `.1` is the TOKEN: text is inserted literally; the special
/// tokens are C (clear), BS (backspace), NEG (negate), MC/MR/M+/M-.
pub const BUTTONS: [(&str, &str); 36] = [
    ("sin", "sin("), ("cos", "cos("), ("tan", "tan("), ("\u{221A}", "sqrt("), ("ln", "ln("), ("log", "log("),
    ("x^y", "^"), ("exp", "exp("), ("|x|", "abs("), ("\u{03C0}", "pi"), ("e", "e"), ("%", "%"),
    ("MC", "MC"), ("MR", "MR"), ("M+", "M+"), ("M-", "M-"), ("(", "("), (")", ")"),
    ("C", "C"), ("7", "7"), ("8", "8"), ("9", "9"), ("/", "/"), ("\u{232B}", "BS"),
    ("\u{00B1}", "NEG"), ("4", "4"), ("5", "5"), ("6", "6"), ("*", "*"), ("-", "-"),
    ("0", "0"), ("1", "1"), ("2", "2"), ("3", "3"), (".", "."), ("+", "+"),
];
/// '=' gets its own wide bar under the grid (commits to history).
const COLS: usize = 6;
const ROWS: usize = 6;
/// content[] layout: 0=expr, 1=result, 2=memory, 3..=history ("expr = result", newest first).
const HIST_MAX: usize = 8;
const HIST_W: usize = 210; // history panel on the right

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
    let eq_h = 44usize; // the wide '=' bar under the grid
    (
        x + pad,
        y + disp_h + pad,
        w.saturating_sub(pad * 2 + HIST_W),
        h.saturating_sub(disp_h + pad * 2 + eq_h),
    )
}

/// The wide '=' bar under the keypad.
fn equals_rect(x: usize, y: usize, w: usize, h: usize) -> (usize, usize, usize, usize) {
    let (ax, ay, aw, ah) = button_area(x, y, w, h);
    (ax, ay + ah + 8, aw, 36)
}

/// Which button-token lies under (mx,my)? `win_*` = full window geometry.
/// "=" for the equals bar; "H0".."H7" for a history row (recall it).
pub fn button_at(win_x: usize, win_y: usize, win_w: usize, win_h: usize, mx: usize, my: usize) -> Option<&'static str> {
    // The '=' bar.
    {
        let (ex, ey, ew, eh) = equals_rect(win_x, win_y + TITLEBAR_H, win_w, win_h.saturating_sub(TITLEBAR_H));
        if mx >= ex && mx < ex + ew && my >= ey && my < ey + eh {
            return Some("=");
        }
    }
    // History rows (right panel).
    {
        let hx = win_x + win_w - HIST_W + 4;
        let hy0 = win_y + TITLEBAR_H + 44;
        if mx >= hx && my >= hy0 && my < hy0 + HIST_MAX * 30 {
            const H: [&str; 8] = ["H0", "H1", "H2", "H3", "H4", "H5", "H6", "H7"];
            let i = (my - hy0) / 30;
            if i < 8 {
                return Some(H[i]);
            }
        }
    }
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
    fb.fill_rounded_rect(x + pad, y + pad, w - pad * 2 - HIST_W, disp_h - 8, crate::eds::RADIUS_M, Color::rgb(0x20, 0x2A, 0x36));
    let empty = alloc::string::String::new();
    let expr = content.first().unwrap_or(&empty);
    let result = content.get(1).unwrap_or(&empty);
    // Expression (small, light gray, right-aligned).
    let ew = text::width_px(expr, 16.0);
    let exr = (x + w - pad - 14 - HIST_W).saturating_sub(ew);
    text::draw_px(fb, exr.max(x + pad + 14), y + pad + 14, expr, Color::rgb(0x9A, 0xA6, 0xB4), 16.0);
    // Result (large, white, right-aligned).
    let disp_res = if result.is_empty() { "0" } else { result.as_str() };
    let rw = text::width_px(disp_res, 34.0);
    let rxr = (x + w - pad - 14 - HIST_W).saturating_sub(rw);
    text::draw_px(fb, rxr.max(x + pad + 14), y + pad + 40, disp_res, Color::WHITE, 34.0);

    // ── Buttons (6×6: scientific + memory + classic) ──
    let (ax, ay, aw, ah) = button_area(x, y, w, h);
    for (i, (label, tok)) in BUTTONS.iter().enumerate() {
        let (rx, ry, rw_, rh) = button_rect(i, ax, ay, aw, ah);
        let (bg, fg, size) = match *tok {
            "+" | "-" | "*" | "/" | "(" | ")" | "^" | "%" => (Color::ACCENT_SOFT, Color::ACCENT, 17.0),
            "C" => (Color::rgb(0xFD, 0xEA, 0xE8), Color::rgb(0xD6, 0x45, 0x3D), 17.0),
            "BS" => (Color::SURFACE_3, Color::TEXT_SEC, 17.0),
            "MC" | "MR" | "M+" | "M-" => (Color::rgb(0xE9, 0xE2, 0xF7), Color::rgb(0x6B, 0x46, 0xB8), 14.0),
            t if t.ends_with('(') || t == "pi" || t == "e" => (Color::SURFACE_3, Color::TEXT_SEC, 14.0),
            _ => (Color::CARD, Color::INK, 17.0),
        };
        fb.fill_rounded_rect(rx, ry, rw_, rh, crate::eds::RADIUS_M, bg);
        if matches!(*tok, "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | ".") {
            fb.draw_border(rx, ry, rw_, rh, 1, Color::BORDER);
        }
        let lw = text::width_px(label, size);
        text::draw_px(fb, rx + (rw_.saturating_sub(lw)) / 2, ry + (rh.saturating_sub(size as usize)) / 2, label, fg, size);
    }
    // The wide '=' bar.
    let (ex, ey, ew, eh) = equals_rect(x, y, w, h);
    fb.fill_rounded_rect(ex, ey, ew, eh, crate::eds::RADIUS_M, Color::ACCENT);
    let eqw = text::width_px("=", 20.0);
    text::draw_px(fb, ex + (ew - eqw) / 2, ey + 7, "=", Color::WHITE, 20.0);

    // ── Memory chip + history panel (right) ──
    let hx = x + w - HIST_W + 4;
    fb.fill_rect(hx - 8, y, 1, h, Color::BORDER);
    let mem = content.get(2).map(|s| s.as_str()).unwrap_or("");
    let mem_lbl = if mem.is_empty() { alloc::string::String::from("M: empty") } else { alloc::format!("M: {mem}") };
    text::draw_px(fb, hx + 6, y + 12, &mem_lbl, Color::TEXT_SEC, 12.5);
    text::draw_px(fb, hx + 6, y + 30, "History", Color::TEXT_DIM, 11.0);
    let hy0 = y + 44;
    for i in 0..HIST_MAX {
        let Some(line) = content.get(3 + i) else { break };
        let ry = hy0 + i * 30;
        fb.fill_rounded_rect(hx, ry, HIST_W - 16, 26, 6, Color::CARD);
        // Clip long lines from the left so the RESULT stays visible.
        let mut shown = line.clone();
        while text::width_px(&shown, 12.0) > HIST_W - 34 && shown.chars().count() > 4 {
            shown.remove(0);
        }
        text::draw_px(fb, hx + 8, ry + 6, &shown, Color::INK, 12.0);
    }
}

/// Process one input character against the calculator state. Adjusts `content` and
/// recomputes the REAL result via the euroreken engine. Returns true if
/// something changed (so the loop can redraw).
pub fn input(content: &mut alloc::vec::Vec<alloc::string::String>, ch: char) -> bool {
    // Keyboard path: single characters map onto tokens.
    let tok: alloc::string::String = match ch {
        'C' => alloc::string::String::from("C"),
        '\u{8}' => alloc::string::String::from("BS"),
        '=' | '\r' | '\n' => alloc::string::String::from("="),
        c => {
            let mut t = alloc::string::String::new();
            t.push(c);
            t
        }
    };
    input_token(content, &tok)
}

/// Process one TOKEN (button or key). Adjusts `content` and recomputes the REAL
/// result via the euroreken engine. Returns true if something changed.
pub fn input_token(content: &mut alloc::vec::Vec<alloc::string::String>, tok: &str) -> bool {
    while content.len() < 3 {
        content.push(alloc::string::String::new());
    }
    match tok {
        "C" => content[0].clear(),
        "BS" => {
            content[0].pop();
        }
        "NEG" => {
            // Negate: wrap the whole expression (simple, always correct).
            if !content[0].is_empty() {
                content[0] = alloc::format!("-({})", content[0]);
            }
        }
        "=" => {
            // Commit to history: "expr = result" (newest first, HIST_MAX deep).
            let expr = alloc::string::String::from(content[0].trim());
            if !expr.is_empty() && content[1] != "\u{2026}" {
                let line = alloc::format!("{expr} = {}", content[1]);
                content.insert(3, line);
                content.truncate(3 + HIST_MAX);
            }
        }
        "MC" => content[2].clear(),
        "MR" => {
            let m = content[2].clone();
            content[0].push_str(&m);
        }
        "M+" | "M-" => {
            // memory := memory ± current result.
            // Use the LIVE result (already paren-balanced) as the current value.
            let cur: f64 = euroreken::eval(&content[1]).unwrap_or(0.0);
            let mem: f64 = if content[2].is_empty() { 0.0 } else { euroreken::eval(&content[2]).unwrap_or(0.0) };
            let newm = if tok == "M+" { mem + cur } else { mem - cur };
            content[2] = fmt_num(newm);
        }
        t if t.starts_with('H') => {
            // Recall history row N: put its expression back in the input.
            if let Some(i) = t[1..].parse::<usize>().ok() {
                if let Some(line) = content.get(3 + i) {
                    if let Some((expr, _)) = line.rsplit_once(" = ") {
                        content[0] = alloc::string::String::from(expr);
                    }
                }
            }
        }
        text => content[0].push_str(text),
    }
    // REAL evaluation via the euroreken engine. Unclosed parentheses are
    // auto-balanced for the LIVE result (calculator convention: "sqrt(9" -> 3).
    let mut expr = content[0].clone();
    let open = expr.matches('(').count();
    let close = expr.matches(')').count();
    for _ in close..open {
        expr.push(')');
    }
    content[1] = if expr.trim().is_empty() {
        alloc::string::String::from("0")
    } else {
        match euroreken::eval(&expr) {
            Ok(v) => fmt_num(v),
            Err(_) => alloc::string::String::from("\u{2026}"),
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
