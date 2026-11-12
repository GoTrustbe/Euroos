//! Kernel side of **EuroFiles** (Sprint AC-1): the file manager.
//! At boot we prove directory sorting (directories first), filtering, path normalization
//! and the sovereign badges. Host-tested core: [`eurofiles`].
//! Also contains the desktop GUI (`render`): a "Places" sidebar + a live list
//! of the REAL EuroFS (the kernel fills it via `load_dir` from `fs.list_dir`).

use crate::graphics::{Color, FrameBuffer};
use crate::serial_println;
use crate::{icons, text};
use alloc::string::String;
use alloc::vec::Vec;
use eurofiles::{human_size, join, normalize, parent, Badge, DirEntry, FileKind, Listing, SortKey, SortOrder};
use spin::Mutex;

/// Equal to `compositor::TITLEBAR_H`.
const TITLEBAR_H: usize = 44;
const PLACES_W: usize = 156;
const ROW_H: usize = 30;
const PLACE_H: usize = 40;

/// Shortcuts in the sidebar (label, path, glyph).
const PLACES: &[(&str, &str, &str)] = &[
    ("Home", "/home/euro", "home"),
    ("System", "/", "files"),
    ("Configuration", "/etc", "settings"),
    ("Logs", "/var", "doc"),
];

/// The LIVE directory list that the GUI shows (filled by the kernel from the real FS).
static LISTING: Mutex<Option<Listing>> = Mutex::new(None);

use core::sync::atomic::{AtomicUsize, Ordering};
/// Selected row index in the listing (usize::MAX = nothing selected).
static SELECTED: AtomicUsize = AtomicUsize::new(usize::MAX);
/// Index of the first listing entry shown: the list scrolls with the mouse wheel.
static LIST_TOP: AtomicUsize = AtomicUsize::new(0);
/// Clipboard: (absolute path, is_cut). Copy leaves it; Cut removes on paste.
static CLIPBOARD: Mutex<Option<(String, bool)>> = Mutex::new(None);

/// A pending filesystem mutation the kernel loop flushes with FS access.
pub enum FileOp {
    NewDir(String),
    Chmod(String, u16),
    ToggleProtect(String),
    ShowHistory(String),
    RestoreVersion(String, u32),
    Rename(String, String),
    Delete(String, bool), // (path, is_dir)
    Copy(String, String),
    Move(String, String),
}
static PENDING: Mutex<Vec<FileOp>> = Mutex::new(Vec::new());

/// A name-entry prompt (New Folder / Rename).
struct Prompt {
    mode: PromptMode,
    buf: crate::editcore::Buffer,
    orig: String, // for Rename: the original path
}
#[derive(PartialEq)]
enum PromptMode {
    NewDir,
    Rename,
    Perms,
}
static PROMPT: Mutex<Option<Prompt>> = Mutex::new(None);

/// The open History panel: (path, versions newest-first as (n, size, mtime)).
static HISTORY: Mutex<Option<(String, Vec<(u32, u64, u64)>)>> = Mutex::new(None);

/// The kernel answers a History request by filling the panel.
pub fn show_history(path: &str, rows: Vec<(u32, u64, u64)>) {
    *HISTORY.lock() = Some((String::from(path), rows));
}
pub fn history_open() -> bool {
    HISTORY.lock().is_some()
}

/// A click while the History panel is open: a row restores that version,
/// anywhere else closes the panel. Returns Some((path, n)) to restore.
pub fn history_click(win_x: usize, win_y: usize, mx: usize, my: usize, win_w: usize, win_h: usize) -> Option<(String, u32)> {
    let mut g = HISTORY.lock();
    let Some((path, rows)) = g.as_ref() else { return None };
    let pw = 420usize;
    let ph = 90 + rows.len().min(8) * 28;
    let ox = win_x + win_w.saturating_sub(pw) / 2;
    let oy = win_y + TITLEBAR_H + win_h.saturating_sub(TITLEBAR_H + ph) / 2;
    if mx >= ox && mx < ox + pw && my >= oy + 44 && my < oy + 44 + rows.len().min(8) * 28 {
        let i = (my - (oy + 44)) / 28;
        if let Some((n, _, _)) = rows.get(i).copied() {
            let p = path.clone();
            *g = None;
            return Some((p, n));
        }
    }
    *g = None; // click elsewhere dismisses
    None
}

/// The toolbar actions (label shown on the button).
const ACTIONS: [&str; 9] = ["New Folder", "Rename", "Delete", "Copy", "Cut", "Paste", "Perms", "Protect", "History"];

/// The toolbar buttons that fit in a list column `list_w` wide starting at
/// `list_x`: (x, width) per action, `None` for one that does not fit. Each
/// button is as wide as its label, so all nine fit the default window (the
/// last three, Perms, Protect and History, used to fall outside it with a
/// fixed 92 px pitch). Render, click and tooltip all use this one layout.
fn tool_rects(list_x: usize, list_w: usize) -> [Option<(usize, usize)>; ACTIONS.len()] {
    let mut out = [None; ACTIONS.len()];
    let mut x = list_x + 8;
    for (i, label) in ACTIONS.iter().enumerate() {
        let w = text::width_px(label, 11.5) + 18;
        if x + w + 4 > list_x + list_w {
            break;
        }
        out[i] = Some((x, w));
        x += w + 5;
    }
    out
}

/// The action whose button is under `mx`.
fn tool_at(list_x: usize, list_w: usize, mx: usize) -> Option<usize> {
    tool_rects(list_x, list_w).iter().position(|r| matches!(r, Some((x, w)) if mx >= *x && mx < *x + *w))
}

/// How many listing entries fit in a window `win_h` high (below the ".." row,
/// above the status bar).
fn rows_fit(win_h: usize) -> usize {
    (win_h.saturating_sub(TITLEBAR_H + 90 + 26) / ROW_H).saturating_sub(1)
}

/// Mouse wheel over the window: scroll the list by `notches` (positive = down),
/// three rows per notch. Returns true when the view moved.
pub fn wheel(win_h: usize, notches: i32) -> bool {
    let n = LISTING.lock().as_ref().map(|l| l.entries.len()).unwrap_or(0);
    let max_top = n.saturating_sub(rows_fit(win_h).max(1));
    let cur = LIST_TOP.load(Ordering::Relaxed).min(max_top);
    let new = (cur as i64 + notches as i64 * 3).clamp(0, max_top as i64) as usize;
    LIST_TOP.store(new, Ordering::Relaxed);
    new != cur
}

/// A Unix-style permission string like "drwxr-xr-x" from a mode + kind.
fn perm_string(mode: u16, is_dir: bool) -> String {
    let bit = |m: u16, b: char| if mode & m != 0 { b } else { '-' };
    let mut s = String::new();
    s.push(if is_dir { 'd' } else { '-' });
    s.push(bit(0o400, 'r')); s.push(bit(0o200, 'w')); s.push(bit(0o100, 'x'));
    s.push(bit(0o040, 'r')); s.push(bit(0o020, 'w')); s.push(bit(0o010, 'x'));
    s.push(bit(0o004, 'r')); s.push(bit(0o002, 'w')); s.push(bit(0o001, 'x'));
    s
}

fn selected_entry() -> Option<(String, bool)> {
    let i = SELECTED.load(Ordering::Relaxed);
    let g = LISTING.lock();
    let l = g.as_ref()?;
    let e = l.entries.get(i)?;
    Some((join(&l.path, &e.name), e.kind == FileKind::Dir))
}

/// Take the queued operations (the kernel loop runs them with FS access).
pub fn take_ops() -> Vec<FileOp> {
    core::mem::take(&mut *PENDING.lock())
}

/// True while a name-entry prompt has the keyboard.
pub fn prompt_open() -> bool {
    PROMPT.lock().is_some()
}

/// Feed a key to the open prompt. Enter commits, Esc cancels.
pub fn key(k: crate::ps2::Key) {
    let mut g = PROMPT.lock();
    let Some(p) = g.as_mut() else { return };
    match k {
        crate::ps2::Key::Enter => {
            let name = p.buf.text();
            let name = name.trim();
            if !name.is_empty() {
                let cur = current_path();
                let cur = if cur.is_empty() { String::from("/") } else { cur };
                match p.mode {
                    PromptMode::NewDir => PENDING.lock().push(FileOp::NewDir(join(&cur, name))),
                    PromptMode::Rename => PENDING.lock().push(FileOp::Rename(p.orig.clone(), join(&cur, name))),
                    PromptMode::Perms => {
                        if let Ok(m) = u16::from_str_radix(name, 8) {
                            PENDING.lock().push(FileOp::Chmod(p.orig.clone(), m));
                        }
                    }
                }
            }
            *g = None;
        }
        crate::ps2::Key::Esc => *g = None,
        other => {
            p.buf.key(other);
        }
    }
}

/// A click on the action toolbar. Returns true if it handled an action.
pub fn toolbar_click(win_x: usize, win_y: usize, mx: usize, my: usize, win_w: usize) -> bool {
    let list_x = win_x + PLACES_W + 1;
    let tb_y = win_y + TITLEBAR_H + 40; // below the path bar
    if my < tb_y || my >= tb_y + 30 {
        return false;
    }
    // Only a button that is drawn takes the click (a narrow window draws fewer).
    let Some(i) = tool_at(list_x, win_w.saturating_sub(PLACES_W + 1), mx) else { return false };
    match ACTIONS[i] {
        "New Folder" => {
            *PROMPT.lock() = Some(Prompt { mode: PromptMode::NewDir, buf: crate::editcore::Buffer::new(), orig: String::new() });
        }
        "Rename" => {
            if let Some((path, _)) = selected_entry() {
                let name = path.rsplit('/').next().map(String::from).unwrap_or_default();
                let mut b = crate::editcore::Buffer::new();
                b.set_text(&name);
                b.col = name.chars().count();
                *PROMPT.lock() = Some(Prompt { mode: PromptMode::Rename, buf: b, orig: path });
            }
        }
        "Delete" => {
            if let Some((path, is_dir)) = selected_entry() {
                PENDING.lock().push(FileOp::Delete(path, is_dir));
                SELECTED.store(usize::MAX, Ordering::Relaxed);
            }
        }
        "Copy" => {
            if let Some((path, _)) = selected_entry() {
                *CLIPBOARD.lock() = Some((path, false));
            }
        }
        "Cut" => {
            if let Some((path, _)) = selected_entry() {
                *CLIPBOARD.lock() = Some((path, true));
            }
        }
        "Perms" => {
            if let Some((path, _)) = selected_entry() {
                // Pre-fill with the current octal mode.
                let cur = {
                    let g = LISTING.lock();
                    let i = SELECTED.load(Ordering::Relaxed);
                    g.as_ref().and_then(|l| l.entries.get(i)).map(|e| e.mode).unwrap_or(0o644)
                };
                let mut b = crate::editcore::Buffer::new();
                let txt = alloc::format!("{:o}", cur);
                b.set_text(&txt);
                b.col = txt.chars().count();
                *PROMPT.lock() = Some(Prompt { mode: PromptMode::Perms, buf: b, orig: path });
            }
        }
        "Protect" => {
            if let Some((path, _)) = selected_entry() {
                PENDING.lock().push(FileOp::ToggleProtect(path));
            }
        }
        "History" => {
            if let Some((path, false)) = selected_entry() {
                PENDING.lock().push(FileOp::ShowHistory(path));
            }
        }
        "Paste" => {
            if let Some((src, is_cut)) = CLIPBOARD.lock().clone() {
                let name = src.rsplit('/').next().map(String::from).unwrap_or_default();
                let cur = current_path();
                let cur = if cur.is_empty() { String::from("/") } else { cur };
                let dst = join(&cur, &name);
                if is_cut {
                    PENDING.lock().push(FileOp::Move(src, dst));
                    *CLIPBOARD.lock() = None;
                } else {
                    PENDING.lock().push(FileOp::Copy(src, dst));
                }
            }
        }
        _ => {}
    }
    true
}

/// Select the file/dir row at a click (highlight). Returns true if it hit a row.
pub fn select_row(win_x: usize, win_y: usize, mx: usize, my: usize) -> bool {
    let list_x = win_x + PLACES_W + 1;
    let list_y0 = win_y + TITLEBAR_H + 90; // below path bar + toolbar + header
    if mx < list_x {
        return false;
    }
    let g = LISTING.lock();
    let Some(l) = g.as_ref() else { return false };
    if my < list_y0 + ROW_H {
        return false; // the ".." row
    }
    let idx = (my - list_y0) / ROW_H + LIST_TOP.load(Ordering::Relaxed);
    if idx >= 1 && idx - 1 < l.entries.len() {
        SELECTED.store(idx - 1, Ordering::Relaxed);
        return true;
    }
    false
}

/// The path that is currently shown (empty = nothing loaded yet).
pub fn current_path() -> String {
    LISTING.lock().as_ref().map(|l| l.path.clone()).unwrap_or_default()
}

/// Fill the list with a real directory: `items` = (name, is_dir, size) from
/// `fs.list_dir`. We sort directories-first via the `eurofiles` engine.
pub fn load_dir(path: &str, items: Vec<(String, bool, u64, u64, u16, bool)>) {
    let entries: Vec<DirEntry> = items
        .into_iter()
        .map(|(name, is_dir, size, mtime, mode, protected)| {
            let e = if is_dir { DirEntry::dir(&name) } else { DirEntry::file(&name, size) };
            // Real metadata: modified time + mode + the REAL immutable flag.
            let e = e.modified_at(mtime).with_mode(mode);
            if protected { e.with_badge(Badge::Immutable) } else { e }
        })
        .collect();
    let mut l = Listing::new(&normalize(path), entries);
    l.sort(SortKey::Name, SortOrder::Asc); // directories first, then alphabetically
    let mut g = LISTING.lock();
    // Another folder starts at its top; a reload of the same one keeps its place.
    if g.as_ref().map(|old| old.path != l.path).unwrap_or(true) {
        LIST_TOP.store(0, Ordering::Relaxed);
    }
    *g = Some(l);
}

/// Click handling: return the path where the user wants to go (directory or
/// shortcut or ".." up), or `None`. The kernel then loads that path.
pub fn hit_test(win_x: usize, win_y: usize, mx: usize, my: usize) -> Option<String> {
    let by = win_y + TITLEBAR_H;
    // Places sidebar.
    if mx >= win_x && mx < win_x + PLACES_W && my >= by + 44 {
        let i = (my - (by + 44)) / PLACE_H;
        if i < PLACES.len() {
            return Some(String::from(PLACES[i].1));
        }
        return None;
    }
    // Main list: row 0 = "..", row 1.. = entries.
    let list_x = win_x + PLACES_W;
    let list_y = by + 90; // path bar + action toolbar + column header
    if mx >= list_x && my >= list_y {
        let row = (my - list_y) / ROW_H;
        let cur = current_path();
        if row == 0 {
            let p = parent(&cur);
            return Some(if p.is_empty() { String::from("/") } else { p });
        }
        let guard = LISTING.lock();
        if let Some(l) = guard.as_ref() {
            let idx = row - 1 + LIST_TOP.load(Ordering::Relaxed);
            if let Some(e) = l.entries.get(idx) {
                if e.kind == FileKind::Dir {
                    return Some(join(&cur, &e.name));
                }
            }
        }
    }
    None
}

/// 3F-5: like [`hit_test`] but returns the path of a clicked **file** (not a
/// directory) — so the file manager can open it with its default app.
pub fn hit_test_file(win_x: usize, win_y: usize, mx: usize, my: usize) -> Option<String> {
    let by = win_y + TITLEBAR_H;
    let list_x = win_x + PLACES_W;
    let list_y = by + 90; // path bar + action toolbar + column header
    if mx < list_x || my < list_y {
        return None;
    }
    let row = (my - list_y) / ROW_H;
    if row == 0 {
        return None; // ".." row
    }
    let cur = current_path();
    let guard = LISTING.lock();
    let l = guard.as_ref()?;
    let e = l.entries.get(row - 1 + LIST_TOP.load(Ordering::Relaxed))?;
    if e.kind == FileKind::File {
        Some(join(&cur, &e.name))
    } else {
        None
    }
}

/// Desktop GUI: sidebar with places + the live directory list of the real EuroFS.
pub fn render(fb: &FrameBuffer, x: usize, y: usize, w: usize, h: usize) {
    let bx = x;
    let by = y + TITLEBAR_H;
    let bw = w;
    let bh = h.saturating_sub(TITLEBAR_H);
    let accent = Color::rgb(0x20, 0x59, 0xC8); // EuroFiles blue
    fb.fill_rect(bx, by, bw, bh, Color::SURFACE);

    // ── Places sidebar ───────────────────────────────────────────────────────
    fb.fill_rect(bx, by, PLACES_W, bh, Color::CARD);
    fb.fill_rect(bx + PLACES_W, by, 1, bh, Color::BORDER);
    text::draw_px(fb, bx + 16, by + 16, "Places", Color::TEXT_SEC, 12.5);
    let cur = current_path();
    for (i, (label, path, glyph)) in PLACES.iter().enumerate() {
        let ry = by + 44 + i * PLACE_H;
        let active = &cur == path || (cur.is_empty() && i == 0);
        if active {
            fb.fill_rounded_rect(bx + 8, ry - 4, PLACES_W - 16, PLACE_H - 6, 9, Color::ACCENT_SOFT);
        }
        let col = if active { accent } else { Color::INK };
        icons::draw(fb, glyph, bx + 18, ry + 1, 16, col);
        text::draw_px(fb, bx + 42, ry + 2, label, col, 13.0);
    }

    // ── Path bar ─────────────────────────────────────────────────────────────
    let list_x = bx + PLACES_W + 1;
    let list_w = bw.saturating_sub(PLACES_W + 1);
    fb.fill_rect(list_x, by, list_w, 40, Color::SURFACE);
    fb.fill_rect(list_x, by + 39, list_w, 1, Color::BORDER);
    let shown_path = if cur.is_empty() { "/" } else { &cur };
    icons::draw(fb, "path", list_x + 16, by + 12, 15, Color::TEXT_SEC);
    text::draw_px(fb, list_x + 40, by + 13, shown_path, Color::INK, 13.5);

    // ── Action toolbar ───────────────────────────────────────────────────────
    let tb_y = by + 40;
    fb.fill_rect(list_x, tb_y, list_w, 30, Color::CARD);
    fb.fill_rect(list_x, tb_y + 29, list_w, 1, Color::BORDER);
    let has_sel = SELECTED.load(Ordering::Relaxed) != usize::MAX;
    let has_clip = CLIPBOARD.lock().is_some();
    let tools = tool_rects(list_x, list_w);
    for (i, label) in ACTIONS.iter().enumerate() {
        let Some((bxp, bwid)) = tools[i] else { break };
        // Grey out actions that need a selection / clipboard.
        let enabled = match *label {
            "Rename" | "Delete" | "Copy" | "Cut" | "Perms" | "Protect" | "History" => has_sel,
            "Paste" => has_clip,
            _ => true,
        };
        let bg = if enabled { Color::rgb(0xEA, 0xEE, 0xF4) } else { Color::rgb(0xF2, 0xF4, 0xF7) };
        let fgc = if enabled { accent } else { Color::TEXT_DIM };
        fb.fill_rounded_rect(bxp, tb_y + 4, bwid, 22, 6, bg);
        text::draw_px(fb, bxp + 9, tb_y + 8, label, fgc, 11.5);
    }

    // Column headers (Name / Modified / Size).
    let hdr_y = by + 70;
    fb.fill_rect(list_x, hdr_y, list_w, 20, Color::rgb(0xF6, 0xF7, 0xFA));
    // A narrow window drops the Modified column instead of writing it over the names.
    let show_date = list_w >= 430;
    text::draw_px(fb, list_x + 44, hdr_y + 3, "Name", Color::TEXT_SEC, 11.0);
    if show_date {
        text::draw_px(fb, list_x + list_w - 230, hdr_y + 3, "Modified", Color::TEXT_SEC, 11.0);
    }
    text::draw_px(fb, (list_x + list_w).saturating_sub(60), hdr_y + 3, "Size", Color::TEXT_SEC, 11.0);
    fb.fill_rect(list_x, hdr_y + 20, list_w, 1, Color::BORDER);

    // ── File list ────────────────────────────────────────────────────────────
    let guard = LISTING.lock();
    let list_y0 = by + 90;
    let ymax = (by + bh).saturating_sub(26);
    // ".." row to go up.
    text::draw_px(fb, list_x + 44, list_y0 + 8, "..", Color::TEXT_SEC, 13.0);
    // BUG-070: the "folder" glyph is an SVG path with arcs, which the icon
    // rasteriser does not draw (it came out as a slanted hook); "files" is the
    // folder shape made of straight segments that the Places column uses.
    icons::draw(fb, "files", list_x + 18, list_y0 + 6, 16, Color::TEXT_DIM);

    let (mut ndirs, mut nfiles, mut total) = (0usize, 0usize, 0u64);
    if let Some(l) = guard.as_ref() {
        let (d, f) = l.counts();
        ndirs = d;
        nfiles = f;
        total = l.total_size();
        // The wheel scrolls the entries under the fixed ".." row.
        let fit = rows_fit(h).max(1);
        let top = LIST_TOP.load(Ordering::Relaxed).min(l.entries.len().saturating_sub(fit));
        LIST_TOP.store(top, Ordering::Relaxed);
        if l.entries.len() > fit {
            // Scroll position: a thin thumb at the right edge of the list.
            let track_y = list_y0 + ROW_H;
            let track_h = ymax.saturating_sub(track_y);
            let thumb_h = (track_h * fit / l.entries.len()).max(16).min(track_h);
            let thumb_y = track_y + (track_h - thumb_h) * top / (l.entries.len() - fit);
            fb.fill_rect((list_x + list_w).saturating_sub(4), thumb_y, 3, thumb_h, Color::BORDER);
        }
        for (i, e) in l.entries.iter().enumerate().skip(top) {
            let ry = list_y0 + (i - top + 1) * ROW_H;
            if ry + ROW_H > ymax {
                break;
            }
            let is_dir = e.kind == FileKind::Dir;
            if SELECTED.load(Ordering::Relaxed) == i {
                fb.fill_rounded_rect(list_x + 6, ry + 1, list_w - 12, ROW_H - 4, 6, Color::ACCENT_SOFT);
            }
            let (glyph, gcol) = if is_dir { ("files", accent) } else { ("doc", Color::TEXT_SEC) };
            icons::draw(fb, glyph, list_x + 18, ry + 6, 16, gcol);
            text::draw_px(fb, list_x + 44, ry + 8, &e.name, Color::INK, 13.0);
            // Modified date (middle column) + size (right column).
            if show_date {
                let date = crate::rtc::short_datetime(e.modified);
                text::draw_px(fb, list_x + list_w - 230, ry + 9, &date, Color::TEXT_DIM, 11.5);
            }
            let mut rx = list_x + list_w;
            if !is_dir {
                let sz = human_size(e.size);
                let sw = text::width_px(&sz, 11.5);
                text::draw_px(fb, list_x + list_w - sw - 16, ry + 9, &sz, Color::TEXT_DIM, 11.5);
                rx = list_x + list_w - sw - 28;
            } else {
                text::draw_px(fb, list_x + list_w - 44, ry + 9, "dir", Color::TEXT_DIM, 11.5);
                rx = list_x + list_w - 56;
            }
            for b in &e.badges {
                if b == &Badge::Immutable {
                    let lbl = "protected";
                    let cw = text::width_px(lbl, 10.5) + 16;
                    fb.fill_rounded_rect(rx - cw, ry + 5, cw, 18, 9, Color::rgb(0xFB, 0xEC, 0xD9));
                    text::draw_px(fb, rx - cw + 8, ry + 7, lbl, Color::rgb(0xB0, 0x62, 0x10), 10.5);
                    rx -= cw + 6;
                }
                if b == &Badge::Signed {
                    let lbl = "signed";
                    let cw = text::width_px(lbl, 10.5) + 16;
                    fb.fill_rounded_rect(rx - cw, ry + 5, cw, 18, 9, Color::ACCENT_SOFT);
                    text::draw_px(fb, rx - cw + 8, ry + 7, lbl, accent, 10.5);
                    rx -= cw + 6;
                }
            }
        }
    } else {
        text::draw_px(fb, list_x + 44, list_y0 + ROW_H + 8, "(directory loading\u{2026})", Color::TEXT_DIM, 12.5);
    }
    drop(guard); // release the list lock before any further LISTING access

    // ── Status bar ───────────────────────────────────────────────────────────
    let sy = (by + bh).saturating_sub(26);
    fb.fill_rect(bx, sy, bw, 26, accent);
    text::draw_px(fb, bx + 14, sy + 6, "EuroFiles  ·  live EuroFS", Color::WHITE, 11.5);
    // Properties strip for the selected entry, just above the status bar (single lock).
    {
        let g2 = LISTING.lock();
        if let Some(l) = g2.as_ref() {
            let i = SELECTED.load(Ordering::Relaxed);
            if let Some(e) = l.entries.get(i) {
                let is_dir = e.kind == FileKind::Dir;
                let path = join(&l.path, &e.name);
                let py = (by + bh).saturating_sub(26 + 22);
                fb.fill_rect(list_x, py, list_w, 22, Color::rgb(0xF0, 0xF2, 0xF6));
                let perms = perm_string(e.mode, is_dir);
                let info = if is_dir {
                    alloc::format!("{}  \u{00B7}  folder  \u{00B7}  {}  \u{00B7}  modified {}",
                        path, perms, crate::rtc::short_datetime(e.modified))
                } else {
                    alloc::format!("{}  \u{00B7}  {}  \u{00B7}  {}  \u{00B7}  modified {}",
                        path, human_size(e.size), perms, crate::rtc::short_datetime(e.modified))
                };
                text::draw_px(fb, list_x + 12, py + 4, &info, Color::TEXT_SEC, 11.5);
            }
        }
    }

    let right = alloc::format!("{} directories \u{00B7} {} files \u{00B7} {}", ndirs, nfiles, human_size(total));
    let rw = text::width_px(&right, 11.5);
    // The counts go where there is room next to the label on the left.
    if bw >= rw + 14 + 190 {
        text::draw_px(fb, bx + bw - rw - 14, sy + 6, &right, Color::WHITE, 11.5);
    }

    // ── History panel: stored versions of the selected file ─────────────────
    if let Some((path, rows)) = HISTORY.lock().as_ref() {
        let pw = 420usize;
        let ph = 90 + rows.len().min(8) * 28;
        let ox = bx + bw.saturating_sub(pw) / 2;
        let oy = by + bh.saturating_sub(ph) / 2;
        fb.fill_rounded_rect(ox - 2, oy - 2, pw + 4, ph + 4, 12, Color::rgb(0, 0, 0));
        fb.fill_rounded_rect(ox, oy, pw, ph, 10, Color::SURFACE);
        let name = path.rsplit('/').next().unwrap_or(path);
        text::draw_px(fb, ox + 18, oy + 12, &alloc::format!("History of {name}"), Color::INK, 13.5);
        if rows.is_empty() {
            text::draw_px(fb, ox + 18, oy + 48, "no stored versions yet", Color::TEXT_DIM, 12.5);
        }
        for (i, (n, size, mtime)) in rows.iter().take(8).enumerate() {
            let ry = oy + 44 + i * 28;
            fb.fill_rounded_rect(ox + 12, ry, pw - 24, 24, 6, Color::rgb(0xEF, 0xF1, 0xF6));
            let line = alloc::format!("v{n}  -  {}  -  {}", human_size(*size), crate::rtc::short_datetime(*mtime));
            text::draw_px(fb, ox + 22, ry + 5, &line, Color::INK, 12.0);
            text::draw_px(fb, ox + pw - 78, ry + 5, "restore", accent, 12.0);
        }
        text::draw_px(fb, ox + 18, oy + ph - 26, "click a version to restore it - click elsewhere to close", Color::TEXT_DIM, 10.5);
    }

    // ── Name-entry prompt (New Folder / Rename) ──────────────────────────────
    if let Some(p) = PROMPT.lock().as_ref() {
        let pw = 360usize;
        let ph = 96usize;
        let ox = bx + bw.saturating_sub(pw) / 2;
        let oy = by + bh.saturating_sub(ph) / 2;
        fb.fill_rounded_rect(ox - 2, oy - 2, pw + 4, ph + 4, 12, Color::rgb(0, 0, 0));
        fb.fill_rounded_rect(ox, oy, pw, ph, 10, Color::SURFACE);
        let title = match p.mode {
            PromptMode::Rename => "Rename to:",
            PromptMode::Perms => "Permissions (octal, e.g. 644):",
            PromptMode::NewDir => "New folder name:",
        };
        text::draw_px(fb, ox + 18, oy + 14, title, Color::INK, 13.5);
        // Text field with the buffer + a caret.
        fb.fill_rounded_rect(ox + 16, oy + 40, pw - 32, 28, 6, Color::rgb(0xFF, 0xFF, 0xFF));
        fb.draw_border(ox + 16, oy + 40, pw - 32, 28, 1, accent);
        let field = p.buf.text();
        text::draw_px(fb, ox + 24, oy + 46, &field, Color::INK, 13.5);
        let cx = ox + 24 + text::width_px(&field.chars().take(p.buf.col).collect::<String>(), 13.5);
        fb.fill_rect(cx, oy + 45, 2, 18, accent);
        text::draw_px(fb, ox + 18, oy + 74, "Enter = OK  \u{00B7}  Esc = cancel", Color::TEXT_DIM, 11.0);
    }
}

/// Boot self-test: build a directory list, sort/filter, check path operations.
pub fn selftest() {
    let mut l = Listing::new(
        "/etc//euro/../euro",
        alloc::vec![
            DirEntry::file("zoem.txt", 1500),
            DirEntry::dir("conf"),
            DirEntry::file("kernel.efi", 2_500_000)
                .with_badge(Badge::Immutable)
                .with_badge(Badge::Signed),
            DirEntry::file(".verborgen", 10),
            DirEntry::dir("Assets"),
        ],
    );
    l.sort(SortKey::Name, SortOrder::Asc);
    let first_two: alloc::vec::Vec<&str> = l.entries.iter().take(2).map(|e| e.name.as_str()).collect();
    let dirs_first = first_two == alloc::vec!["Assets", "conf"];

    let visible = l.filter("", false).len(); // hidden removed → 4
    let hits = l.filter("kernel", true).len(); // 1
    let (dirs, files) = l.counts();

    let path_ok = l.path == "/etc/euro"
        && normalize("/a/b/../c") == "/a/c"
        && join("/home/user", "docs/../x.md") == "/home/user/x.md";

    let kernel_signed = l
        .entries
        .iter()
        .find(|e| e.name == "kernel.efi")
        .map(|e| e.badges.contains(&Badge::Immutable) && e.badges.contains(&Badge::Signed))
        .unwrap_or(false);

    let ok = dirs_first && visible == 4 && hits == 1 && dirs == 2 && files == 3 && path_ok && kernel_signed;
    serial_println!(
        "[fl] EuroFiles: path={}, {} directories/{} files, dirs-first={}, visible={} (of 5), kernel.efi🔒signed={} {}",
        l.path,
        dirs,
        files,
        dirs_first,
        visible,
        kernel_signed,
        if ok { "✓" } else { "✗ FAIL" }
    );
}

/// Tooltip text for the control under (mx,my): the action toolbar and the
/// "protected" badge of a row. Same geometry as `toolbar_click` and `render`.
pub fn tip_at(win_x: usize, win_y: usize, win_w: usize, mx: usize, my: usize) -> Option<&'static str> {
    let list_x = win_x + PLACES_W + 1;
    let list_w = win_w.saturating_sub(PLACES_W + 1);
    let by = win_y + TITLEBAR_H;
    let tb_y = by + 40;
    if my >= tb_y && my < tb_y + 30 {
        let i = tool_at(list_x, list_w, mx)?; // only a button that is drawn
        return Some(match *ACTIONS.get(i)? {
            "New Folder" => "Create a folder here",
            "Rename" => "Rename the selected item",
            "Delete" => "Delete the selection at once, not via the Trash (a folder only when empty)",
            "Copy" => "Remember the selected file; Paste then copies it into the open folder",
            "Cut" => "Remember the selected item; Paste then moves it into the open folder",
            "Paste" => "Copy or move the remembered item into this folder",
            "Perms" => "Change the permission bits (octal, for example 644)",
            "Protect" => "Make the selected file immutable, or remove that again (own files under /home)",
            "History" => "Earlier versions of the selected file; click one to restore it",
            _ => return None,
        });
    }
    // A "protected" badge: right of the name, left of the size (see render).
    let list_y0 = by + 90;
    if mx < list_x || my < list_y0 + ROW_H {
        return None;
    }
    let i = (my - list_y0) / ROW_H - 1 + LIST_TOP.load(Ordering::Relaxed);
    let guard = LISTING.lock();
    let e = guard.as_ref()?.entries.get(i)?;
    if !e.badges.contains(&Badge::Immutable) {
        return None;
    }
    let right = if e.kind == FileKind::Dir {
        list_x + list_w - 56
    } else {
        list_x + list_w - text::width_px(&human_size(e.size), 11.5) - 28
    };
    let cw = text::width_px("protected", 10.5) + 16;
    if mx + cw >= right && mx < right {
        Some("Immutable: cannot be changed, renamed or deleted until Protect removes it")
    } else {
        None
    }
}

/// Part of the `[help]` boot self-test: the toolbar tips line up with ACTIONS.
pub fn tip_selftest() -> bool {
    // All nine buttons are drawn at the default window width (720) and have a tip.
    let rects = tool_rects(PLACES_W + 1, 720 - PLACES_W - 1);
    let at = |i: usize| rects.get(i).copied().flatten().and_then(|(x, _)| tip_at(0, 0, 720, x + 5, TITLEBAR_H + 40 + 10));
    let past_last = rects[ACTIONS.len() - 1].map(|(x, w)| x + w + 3).unwrap_or(0);
    // The smallest window draws fewer buttons, and the missing ones take no click.
    let narrow = tool_rects(PLACES_W + 1, 320 - PLACES_W - 1);
    (0..ACTIONS.len()).all(|i| at(i).is_some())
        && at(7).map(|t| t.contains("immutable")).unwrap_or(false)
        && at(8).map(|t| t.contains("versions")).unwrap_or(false)
        && tip_at(0, 0, 720, past_last, TITLEBAR_H + 40 + 10).is_none()
        && narrow[ACTIONS.len() - 1].is_none()
        && tool_at(PLACES_W + 1, 320 - PLACES_W - 1, 319).is_none()
        && tip_at(0, 0, 1200, PLACES_W + 20, TITLEBAR_H + 10).is_none()
}
