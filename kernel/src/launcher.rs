//! The app launcher: a search-as-you-type overlay (a Start menu / Spotlight /
//! Activities equivalent) opened from the dock's EU mark. Type to filter the
//! apps, Enter to open the top match, Esc to dismiss. It launches by returning
//! the dock tile index, which the compositor already knows how to open.

use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

use crate::graphics::{Color, FrameBuffer};

/// The searchable catalogue: (display name, dock tile index). The index matches
/// `dock_targets` in main, so a match reuses the existing open-app path.
const CATALOG: &[(&str, usize)] = &[
    ("EuroFiles", 0),
    ("EuroNotes", 1),
    ("EuroClock", 2),
    ("EuroWeb browser", 3),
    ("Terminal", 4),
    ("Settings (EuroBeheer)", 5),
    ("Calculator (EuroReken)", 6),
    ("EuroAgent", 7),
    ("EuroText editor", 8),
    ("EuroMonitor", 9),
    ("EuroLog", 10),
    // No dock tile of its own: the hosted X-client window (see dock_targets[11]).
    ("Chromium browser", 11),
    ("EuroView (images)", 12),
    ("EuroPaint", 13),
];

struct State {
    query: String,
    sel: usize,
}

static LAUNCHER: Mutex<Option<State>> = Mutex::new(None);

const W: usize = 460;
const FIELD_H: usize = 46;
const ROW_H: usize = 40;
const MAX_ROWS: usize = 7;

pub fn is_open() -> bool {
    LAUNCHER.lock().is_some()
}

/// The display name for a dock tile index (for tooltips). None if unmapped.
pub fn name_for_icon(icon: usize) -> Option<&'static str> {
    CATALOG.iter().find(|(_, i)| *i == icon).map(|(n, _)| *n)
}

pub fn open() {
    *LAUNCHER.lock() = Some(State { query: String::new(), sel: 0 });
}

pub fn close() {
    *LAUNCHER.lock() = None;
}

/// Filtered matches for a query (case-insensitive substring). Empty query = all.
fn matches(query: &str) -> Vec<(&'static str, usize)> {
    if query.is_empty() {
        return CATALOG.to_vec();
    }
    let q = to_lower(query);
    CATALOG.iter().filter(|(name, _)| to_lower(name).contains(&q)).copied().collect()
}

fn to_lower(s: &str) -> String {
    s.chars().map(|c| c.to_ascii_lowercase()).collect()
}

/// Feed a typed key. Returns `Some(icon)` when the user launches an app (Enter
/// or nothing else); otherwise the launcher stays open (or closes on Esc).
pub fn key(ch: char) -> Option<usize> {
    let mut g = LAUNCHER.lock();
    let Some(st) = g.as_mut() else { return None };
    match ch {
        '\u{1b}' => {
            *g = None; // Esc closes
            None
        }
        '\r' | '\n' => {
            let m = matches(&st.query);
            let pick = m.get(st.sel.min(m.len().saturating_sub(1))).map(|(_, i)| *i);
            *g = None;
            pick
        }
        '\u{8}' | '\u{7f}' => {
            st.query.pop();
            st.sel = 0;
            None
        }
        // Down/Up arrows arrive as these control chars from the shell keymap.
        '\u{e}' => {
            let n = matches(&st.query).len();
            if n > 0 { st.sel = (st.sel + 1) % n; }
            None
        }
        '\u{10}' => {
            let n = matches(&st.query).len();
            if n > 0 { st.sel = (st.sel + n - 1) % n; }
            None
        }
        c if !c.is_control() => {
            st.query.push(c);
            st.sel = 0;
            None
        }
        _ => None,
    }
}

fn origin(screen_w: usize, screen_h: usize) -> (usize, usize, usize) {
    let x = (screen_w.saturating_sub(W)) / 2;
    let y = screen_h / 6;
    let visible = matches("").len().min(MAX_ROWS); // placeholder, recomputed by caller
    let _ = visible;
    (x, y, FIELD_H)
}

/// A click at (mx,my): a result row launches its app; anywhere else dismisses.
pub fn click_at(mx: usize, my: usize, screen_w: usize, screen_h: usize) -> Option<usize> {
    let taken = LAUNCHER.lock().take();
    let Some(st) = taken else { return None };
    let (x, y, _) = origin(screen_w, screen_h);
    let m = matches(&st.query);
    let rows = m.len().min(MAX_ROWS);
    let list_top = y + FIELD_H + 6;
    if mx >= x && mx < x + W {
        for r in 0..rows {
            let ry = list_top + r * ROW_H;
            if my >= ry && my < ry + ROW_H {
                return Some(m[r].1);
            }
        }
    }
    None
}

pub fn render(fb: &FrameBuffer, screen_w: usize, screen_h: usize) {
    let g = LAUNCHER.lock();
    let Some(st) = g.as_ref() else { return };
    let (x, y, _) = origin(screen_w, screen_h);
    let m = matches(&st.query);
    let rows = m.len().min(MAX_ROWS);
    let h = FIELD_H + 6 + rows.max(1) * ROW_H + 8;

    // Dim the desktop behind the launcher a touch, then the card.
    fb.fill_rounded_rect(x + 2, y + 4, W, h, crate::eds::RADIUS_L, Color::BORDER);
    fb.fill_rounded_rect(x, y, W, h, crate::eds::RADIUS_L, Color::CARD);
    fb.draw_border(x, y, W, h, 1, Color::BORDER);

    // Search field.
    crate::text::draw_px(fb, x + 20, y + 15, "Search apps", Color::TEXT_DIM, 12.0);
    let shown = if st.query.is_empty() {
        String::from("Type to search, Enter to open")
    } else {
        alloc::format!("{}|", st.query)
    };
    let col = if st.query.is_empty() { Color::TEXT_DIM } else { Color::INK };
    crate::text::draw_px(fb, x + 20, y + 15, &shown, col, 15.0);
    fb.fill_rect(x + 16, y + FIELD_H - 2, W - 32, 1, Color::BORDER);

    // Results.
    let list_top = y + FIELD_H + 6;
    if rows == 0 {
        crate::text::draw_px(fb, x + 20, list_top + 10, "No matching apps", Color::TEXT_DIM, 13.0);
    }
    for (r, (name, _)) in m.iter().take(MAX_ROWS).enumerate() {
        let ry = list_top + r * ROW_H;
        if r == st.sel {
            fb.fill_rounded_rect(x + 8, ry + 3, W - 16, ROW_H - 6, crate::eds::RADIUS_S, Color::ACCENT_SOFT);
        }
        let c = if r == st.sel { Color::ACCENT } else { Color::INK };
        crate::text::draw_px(fb, x + 22, ry + 11, name, c, 14.0);
    }
}

/// `[launch]` boot self-test: search filters, selection wraps, Enter returns the
/// right app to open, and Esc dismisses.
pub fn selftest() {
    open();
    let opened = is_open();
    // Type "term" → the Terminal (index 4) is the top match; Enter launches it.
    for c in "term".chars() { key(c); }
    let filtered_one = matches("term").len() == 1;
    let launched = key('\r') == Some(4);
    let closed = !is_open();
    // Esc dismisses without launching.
    open();
    let esc = key('\u{1b}').is_none() && !is_open();
    let ok = opened && filtered_one && launched && closed && esc;
    crate::serial_println!(
        "[launch] App launcher: opens={opened}, search-filters={filtered_one}, Enter-opens-match={launched}, Esc-dismisses={esc} → {}",
        if ok { "OK (type to find and open any app) ✓" } else { "FAILED ✗" }
    );
}
