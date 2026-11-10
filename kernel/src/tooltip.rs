//! Tooltips: the little label that appears when the pointer rests on a control.
//! The compositor decides what the cursor is over and, after a short dwell, sets
//! the tooltip text; this module draws it near the cursor. A small thing users
//! only notice when it is missing.

use alloc::string::{String, ToString};
use spin::Mutex;

use crate::graphics::{Color, FrameBuffer};

static TIP: Mutex<Option<(String, usize, usize)>> = Mutex::new(None);

pub fn set(text: &str, x: usize, y: usize) {
    *TIP.lock() = Some((text.to_string(), x, y));
}

pub fn clear() {
    *TIP.lock() = None;
}

pub fn is_shown() -> bool {
    TIP.lock().is_some()
}

pub fn render(fb: &FrameBuffer, screen_w: usize, screen_h: usize) {
    let g = TIP.lock();
    let Some((text, cx, cy)) = g.as_ref() else { return };
    let tw = crate::text::width_px(text, 12.0);
    let w = tw + 20;
    let h = 26;
    // Place below-right of the cursor, clamped on screen.
    let x = (*cx + 16).min(screen_w.saturating_sub(w + 4));
    let y = (*cy + 20).min(screen_h.saturating_sub(h + 4));
    fb.fill_rounded_rect(x + 1, y + 2, w, h, crate::eds::RADIUS_S, Color::BORDER);
    fb.fill_rounded_rect(x, y, w, h, crate::eds::RADIUS_S, Color::INK);
    crate::text::draw_px(fb, x + 10, y + 6, text, Color::rgb(0xF5, 0xF7, 0xFA), 12.0);
}

/// `[tip]` boot self-test: set shows a tooltip, clear removes it.
pub fn selftest() {
    clear();
    let empty = !is_shown();
    set("EuroFiles", 100, 100);
    let shown = is_shown();
    clear();
    let cleared = !is_shown();
    let name_ok = crate::launcher::name_for_icon(4) == Some("Terminal");
    let ok = empty && shown && cleared && name_ok;
    crate::serial_println!(
        "[tip] Tooltips: set-shows={shown}, clear-hides={cleared}, dock-names={name_ok} → {}",
        if ok { "OK (hover labels on controls) ✓" } else { "FAILED ✗" }
    );
}

// ── Tooltips inside apps ─────────────────────────────────────────────────────
//
// The label for a control inside an app window, for controls whose meaning is
// not obvious from their caption. EuroFiles, EuroNotes and EuroView answer for
// their own layout (`tip_at` next to their render code). EuroText, EuroWeb and
// EuroReken are asked through their existing hit tests. EuroPaint has no public
// hit test, so its toolbar layout is mirrored in `paint_tip` below: when the
// EuroPaint toolbar changes, change that function with it.

/// The tooltip for the control of `win` under (px,py), if it has one.
pub fn app_tip(win: &crate::compositor::Window, px: usize, py: usize) -> Option<&'static str> {
    use crate::suite_ui::SuiteApp;
    let top = win.y + crate::compositor::TITLEBAR_H;
    if py < top || px < win.x || px >= win.x + win.w || py >= win.y + win.h {
        return None;
    }
    match win.app {
        SuiteApp::Paint => paint_tip(px - win.x, py - top),
        SuiteApp::Files => crate::files::tip_at(win.x, win.y, win.w, px, py),
        SuiteApp::Notes => crate::notes::tip_at(win.x, win.y, win.w, px, py),
        SuiteApp::ImageView => crate::imageview::tip_at(win.x, win.y, px, py),
        SuiteApp::Text => {
            if crate::textedit::open_button_at(win.x, win.y, win.w, px, py) {
                Some("Open a text file from a list")
            } else if crate::textedit::save_button_at(win.x, win.y, win.w, px, py) {
                Some("Save to the open file (a new text goes to /home/euro/euro.txt)")
            } else {
                None
            }
        }
        // Only the tab strip and the address bar: the page area would need a
        // full page layout per pointer move.
        SuiteApp::Browser if py < top + 70 => match crate::webview::hit_test(win.x, win.y, win.w, px, py) {
            crate::webview::Hit::NewTab => Some("Open a new tab"),
            crate::webview::Hit::UrlBar => Some("Address: type it and press Enter (https:// for an encrypted connection)"),
            _ => None,
        },
        SuiteApp::Reken => calc_tip(crate::calc_ui::button_at(win.x, win.y, win.w, win.h, px, py)?),
        _ => None,
    }
}

fn calc_tip(token: &str) -> Option<&'static str> {
    Some(match token {
        "MC" => "Memory clear",
        "MR" => "Memory recall: put the memory value into the expression",
        "M+" => "Add the current result to the memory",
        "M-" => "Subtract the current result from the memory",
        "NEG" => "Change the sign",
        "BS" => "Delete the last character",
        "C" => "Clear the expression",
        "^" => "Power: x^y",
        "%" => "Remainder of a division (modulo)",
        "=" => "Store this line and its result in History",
        t if t.len() == 2 && t.starts_with('H') => "Click to bring this line back",
        _ => return None,
    })
}

/// EuroPaint toolbar, body-local coordinates. Mirrors paint.rs: tools in a
/// 3 x 3 grid from (10,10), cells 72 x 38; size slider at y 132; colour field
/// at y 178 (176 x 130) with the hue strip at x 190; recent colours at y 322;
/// Undo / Redo / Clear / Save at y 360, 56 px apart. Toolbar width 236.
fn paint_tip(lx: usize, ly: usize) -> Option<&'static str> {
    const TOOLS: [&str; 9] = [
        "Brush: freehand, width from the size slider",
        "Pencil: freehand, 1 pixel wide",
        "Line: press, drag, release",
        "Rectangle outline: press, drag, release",
        "Filled rectangle: press, drag, release",
        "Ellipse: press, drag, release",
        "Bucket: fill the area you click with the colour",
        "Picker: take the colour under the pointer",
        "Eraser: paint white, width from the size slider",
    ];
    if lx >= 236 {
        return None;
    }
    if (10..10 + 3 * 38).contains(&ly) && lx >= 10 {
        let (col, row) = ((lx - 10) / 72, (ly - 10) / 38);
        return if col < 3 { TOOLS.get(row * 3 + col).copied() } else { None };
    }
    if (132..156).contains(&ly) && (12..212).contains(&lx) {
        return Some("Brush size, 1 to 64 pixels");
    }
    if (178..308).contains(&ly) {
        if (8..184).contains(&lx) {
            return Some("Colour: saturation left to right, brightness top to bottom");
        }
        if (190..210).contains(&lx) {
            return Some("Hue");
        }
        return None;
    }
    if (322..346).contains(&ly) {
        return Some("Recent colours: click one to use it again");
    }
    if (360..390).contains(&ly) && lx >= 8 {
        return match (lx - 8) / 56 {
            0 => Some("Undo the last change (up to 24 steps)"),
            1 => Some("Redo"),
            2 => Some("Clear the canvas (can be undone)"),
            3 => Some("Save as /home/euro/pictures/painting.png (always this file, replaces the previous one)"),
            _ => None,
        };
    }
    None
}

/// Part of the `[help]` boot self-test: the in-app table answers for known
/// controls and stays silent elsewhere.
pub fn app_tips_selftest() -> bool {
    let paint = paint_tip(12, 12).map(|t| t.starts_with("Brush")).unwrap_or(false)
        && paint_tip(10 + 72 + 5, 10 + 38 * 2 + 5).map(|t| t.starts_with("Picker")).unwrap_or(false)
        && paint_tip(8 + 3 * 56 + 4, 365).map(|t| t.contains("painting.png")).unwrap_or(false)
        && paint_tip(120, 400).is_none()
        && paint_tip(300, 20).is_none();
    let calc = calc_tip("M+").is_some() && calc_tip("H3").is_some() && calc_tip("7").is_none();
    let files = crate::files::tip_selftest();
    let notes = crate::notes::tip_at(0, 0, 900, 211 + 12 + 46 * 2 + 4, 44 + 8 + 4).map(|t| t.contains("Heading")).unwrap_or(false);
    let view = crate::imageview::tip_at(0, 0, 10 + 86 * 3 + 4, 44 + 6 + 4).map(|t| t.contains("Rotate")).unwrap_or(false);
    paint && calc && files && notes && view
}
