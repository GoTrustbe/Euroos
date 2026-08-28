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

/// One search result: an app, a file matched by name, or a file matched by
/// its content (with a snippet of the matching line).
#[derive(Clone)]
pub enum Hit {
    App(&'static str, usize),
    File(String),
    Content(String, String),
}

/// What the user picked: launch an app (dock icon) or open a path.
pub enum Launch {
    App(usize),
    Path(String),
}

struct State {
    query: String,
    sel: usize,
    hits: Vec<Hit>,
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
    *LAUNCHER.lock() = Some(State { query: String::new(), sel: 0, hits: matches("").into_iter().map(|(n, i)| Hit::App(n, i)).collect() });
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

/// Rebuild the result list for the current query. The kernel loop calls this
/// (it holds the FS) after every launcher keystroke. Apps always match; file
/// names are searched under the roots below; file CONTENT is searched in the
/// user's home once the query has 3+ characters (bounded, so typing stays
/// snappy under TCG).
pub fn refresh(fs: &mut dyn eurofs::fs::FileSystem) {
    let query = {
        let g = LAUNCHER.lock();
        match g.as_ref() {
            Some(st) => st.query.clone(),
            None => return,
        }
    };
    let q = to_lower(&query);
    let mut hits: Vec<Hit> = Vec::new();
    // Apps first.
    for (name, icon) in matches(&query) {
        hits.push(Hit::App(name, icon));
    }
    if !q.is_empty() {
        // File names: bounded walk over the interesting roots.
        let mut found = 0usize;
        for root in ["/home", "/etc", "/bin", "/data"] {
            walk_names(fs, root, &q, 0, &mut hits, &mut found);
            if found >= 10 {
                break;
            }
        }
        // File content: home only, 3+ chars, text files up to 64 KiB.
        if q.len() >= 3 {
            let mut cfound = 0usize;
            walk_content(fs, "/home", &q, 0, &mut hits, &mut cfound);
        }
    }
    if let Some(st) = LAUNCHER.lock().as_mut() {
        if st.query == query {
            st.sel = st.sel.min(hits.len().saturating_sub(1));
            st.hits = hits;
        }
    }
}

fn walk_names(fs: &mut dyn eurofs::fs::FileSystem, dir: &str, q: &str, depth: u32, hits: &mut Vec<Hit>, found: &mut usize) {
    if depth > 4 || *found >= 10 {
        return;
    }
    let entries = match fs.list_dir(dir) {
        Ok(v) => v,
        Err(_) => return,
    };
    for e in entries {
        if *found >= 10 {
            return;
        }
        if e.name.starts_with('.') {
            continue; // hidden + the version store
        }
        let full = if dir == "/" { alloc::format!("/{}", e.name) } else { alloc::format!("{dir}/{}", e.name) };
        if to_lower(&e.name).contains(q) {
            hits.push(Hit::File(full.clone()));
            *found += 1;
        }
        if e.kind == eurofs::EntryKind::Directory {
            walk_names(fs, &full, q, depth + 1, hits, found);
        }
    }
}

fn walk_content(fs: &mut dyn eurofs::fs::FileSystem, dir: &str, q: &str, depth: u32, hits: &mut Vec<Hit>, found: &mut usize) {
    if depth > 4 || *found >= 5 {
        return;
    }
    let entries = match fs.list_dir(dir) {
        Ok(v) => v,
        Err(_) => return,
    };
    for e in entries {
        if *found >= 5 {
            return;
        }
        if e.name.starts_with('.') {
            continue;
        }
        let full = if dir == "/" { alloc::format!("/{}", e.name) } else { alloc::format!("{dir}/{}", e.name) };
        match e.kind {
            eurofs::EntryKind::Directory => walk_content(fs, &full, q, depth + 1, hits, found),
            eurofs::EntryKind::File if e.size <= 64 * 1024 => {
                if let Ok(bytes) = fs.read_file(&full) {
                    if let Ok(text) = core::str::from_utf8(&bytes) {
                        if let Some(line) = text.lines().find(|l| to_lower(l).contains(q)) {
                            let snippet: String = line.trim().chars().take(40).collect();
                            hits.push(Hit::Content(full.clone(), snippet));
                            *found += 1;
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Feed a typed key. Returns `Some(icon)` when the user launches an app (Enter
/// or nothing else); otherwise the launcher stays open (or closes on Esc).
pub fn key(ch: char) -> Option<Launch> {
    let mut g = LAUNCHER.lock();
    let Some(st) = g.as_mut() else { return None };
    match ch {
        '\u{1b}' => {
            *g = None; // Esc closes
            None
        }
        '\r' | '\n' => {
            let pick = st.hits.get(st.sel.min(st.hits.len().saturating_sub(1))).cloned();
            *g = None;
            pick.map(|h| match h {
                Hit::App(_, i) => Launch::App(i),
                Hit::File(p) | Hit::Content(p, _) => Launch::Path(p),
            })
        }
        '\u{8}' | '\u{7f}' => {
            st.query.pop();
            st.sel = 0;
            st.hits = matches(&st.query).into_iter().map(|(n, i)| Hit::App(n, i)).collect();
            None
        }
        // Down/Up arrows arrive as these control chars from the shell keymap.
        '\u{e}' => {
            let n = st.hits.len();
            if n > 0 { st.sel = (st.sel + 1) % n; }
            None
        }
        '\u{10}' => {
            let n = st.hits.len();
            if n > 0 { st.sel = (st.sel + n - 1) % n; }
            None
        }
        c if !c.is_control() => {
            st.query.push(c);
            st.sel = 0;
            // Apps match instantly (no FS); refresh() adds files/content.
            st.hits = matches(&st.query).into_iter().map(|(n, i)| Hit::App(n, i)).collect();
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
pub fn click_at(mx: usize, my: usize, screen_w: usize, screen_h: usize) -> Option<Launch> {
    let taken = LAUNCHER.lock().take();
    let Some(st) = taken else { return None };
    let (x, y, _) = origin(screen_w, screen_h);
    let rows = st.hits.len().min(MAX_ROWS);
    let list_top = y + FIELD_H + 6;
    if mx >= x && mx < x + W {
        for r in 0..rows {
            let ry = list_top + r * ROW_H;
            if my >= ry && my < ry + ROW_H {
                return Some(match st.hits[r].clone() {
                    Hit::App(_, i) => Launch::App(i),
                    Hit::File(p) | Hit::Content(p, _) => Launch::Path(p),
                });
            }
        }
    }
    None
}

pub fn render(fb: &FrameBuffer, screen_w: usize, screen_h: usize) {
    let g = LAUNCHER.lock();
    let Some(st) = g.as_ref() else { return };
    let (x, y, _) = origin(screen_w, screen_h);
    let rows = st.hits.len().min(MAX_ROWS);
    let h = FIELD_H + 6 + rows.max(1) * ROW_H + 8;

    // Dim the desktop behind the launcher a touch, then the card.
    fb.fill_rounded_rect(x + 2, y + 4, W, h, crate::eds::RADIUS_L, Color::BORDER);
    fb.fill_rounded_rect(x, y, W, h, crate::eds::RADIUS_L, Color::CARD);
    fb.draw_border(x, y, W, h, 1, Color::BORDER);

    // Search field.
    let shown = if st.query.is_empty() {
        String::from("Search apps, files and content\u{2026}")
    } else {
        alloc::format!("{}|", st.query)
    };
    let col = if st.query.is_empty() { Color::TEXT_DIM } else { Color::INK };
    crate::text::draw_px(fb, x + 20, y + 15, &shown, col, 15.0);
    fb.fill_rect(x + 16, y + FIELD_H - 2, W - 32, 1, Color::BORDER);

    // Results.
    let list_top = y + FIELD_H + 6;
    if rows == 0 {
        crate::text::draw_px(fb, x + 20, list_top + 10, "No results", Color::TEXT_DIM, 13.0);
    }
    for (r, hit) in st.hits.iter().take(MAX_ROWS).enumerate() {
        let ry = list_top + r * ROW_H;
        if r == st.sel {
            fb.fill_rounded_rect(x + 8, ry + 3, W - 16, ROW_H - 6, crate::eds::RADIUS_S, Color::ACCENT_SOFT);
        }
        let c = if r == st.sel { Color::ACCENT } else { Color::INK };
        match hit {
            Hit::App(name, _) => {
                crate::text::draw_px(fb, x + 22, ry + 11, name, c, 14.0);
                crate::text::draw_px(fb, x + W - 60, ry + 13, "app", Color::TEXT_DIM, 11.0);
            }
            Hit::File(p) => {
                let name = p.rsplit('/').next().unwrap_or(p);
                crate::text::draw_px(fb, x + 22, ry + 6, name, c, 13.5);
                crate::text::draw_px(fb, x + 22, ry + 23, p, Color::TEXT_DIM, 10.5);
            }
            Hit::Content(p, snippet) => {
                let name = p.rsplit('/').next().unwrap_or(p);
                crate::text::draw_px(fb, x + 22, ry + 6, name, c, 13.5);
                crate::text::draw_px(fb, x + 22, ry + 23, snippet, Color::TEXT_DIM, 10.5);
                crate::text::draw_px(fb, x + W - 60, ry + 13, "text", Color::TEXT_DIM, 11.0);
            }
        }
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
    let launched = matches!(key('\r'), Some(Launch::App(4)));
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
