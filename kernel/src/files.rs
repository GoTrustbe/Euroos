//! Kernel side of **EuroFiles** (Sprint AC-1): the file manager.
//! At boot we prove directory sorting (directories first), filtering, path normalization
//! and the sovereign badges. Host-tested core: [`eurofiles`].
//! Also contains the desktop GUI (`render`): a "Places" sidebar + a live list
//! of the REAL EuroFS (the kernel fills it via `load_dir` from `fs.list_dir`).

use crate::graphics::{Color, FrameBuffer};
use crate::serial_println;
use crate::{icons, text};
use alloc::string::{String, ToString};
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
/// Clipboard: (absolute path, is_cut). Copy leaves it; Cut removes on paste.
static CLIPBOARD: Mutex<Option<(String, bool)>> = Mutex::new(None);

/// A pending filesystem mutation the kernel loop flushes with FS access.
pub enum FileOp {
    NewDir(String),
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
}
static PROMPT: Mutex<Option<Prompt>> = Mutex::new(None);

/// The toolbar actions (label shown on the button).
const ACTIONS: [&str; 6] = ["New Folder", "Rename", "Delete", "Copy", "Cut", "Paste"];

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
    let i = mx.checked_sub(list_x + 8).map(|d| d / 92);
    let Some(i) = i.filter(|&i| i < ACTIONS.len()) else { return false };
    let _ = win_w;
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
    let list_y0 = win_y + TITLEBAR_H + 40 + 30; // below path bar + toolbar
    if mx < list_x {
        return false;
    }
    let g = LISTING.lock();
    let Some(l) = g.as_ref() else { return false };
    if my < list_y0 + ROW_H {
        return false; // the ".." row
    }
    let idx = (my - list_y0) / ROW_H;
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
pub fn load_dir(path: &str, items: Vec<(String, bool, u64)>) {
    let entries: Vec<DirEntry> = items
        .into_iter()
        .map(|(name, is_dir, size)| {
            if is_dir {
                DirEntry::dir(&name)
            } else {
                // No "signed" badge here: the file manager does not verify a
                // signature, so we must not imply one from the filename alone.
                // (Boot images ARE Ed25519-verified — but by the loader, not here.)
                DirEntry::file(&name, size)
            }
        })
        .collect();
    let mut l = Listing::new(&normalize(path), entries);
    l.sort(SortKey::Name, SortOrder::Asc); // directories first, then alphabetically
    *LISTING.lock() = Some(l);
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
    let list_y = by + 40;
    if mx >= list_x && my >= list_y {
        let row = (my - list_y) / ROW_H;
        let cur = current_path();
        if row == 0 {
            let p = parent(&cur);
            return Some(if p.is_empty() { String::from("/") } else { p });
        }
        let guard = LISTING.lock();
        if let Some(l) = guard.as_ref() {
            let idx = row - 1;
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
    let list_y = by + 40;
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
    let e = l.entries.get(row - 1)?;
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
    let list_w = bw - PLACES_W - 1;
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
    for (i, label) in ACTIONS.iter().enumerate() {
        let bxp = list_x + 8 + i * 92;
        if bxp + 88 > list_x + list_w { break; }
        // Grey out actions that need a selection / clipboard.
        let enabled = match *label {
            "Rename" | "Delete" | "Copy" | "Cut" => has_sel,
            "Paste" => has_clip,
            _ => true,
        };
        let bg = if enabled { Color::rgb(0xEA, 0xEE, 0xF4) } else { Color::rgb(0xF2, 0xF4, 0xF7) };
        let fgc = if enabled { accent } else { Color::TEXT_DIM };
        fb.fill_rounded_rect(bxp, tb_y + 4, 88, 22, 6, bg);
        text::draw_px(fb, bxp + 10, tb_y + 8, label, fgc, 11.5);
    }

    // ── File list ────────────────────────────────────────────────────────────
    let guard = LISTING.lock();
    let list_y0 = by + 70;
    let ymax = by + bh - 26;
    // ".." row to go up.
    text::draw_px(fb, list_x + 44, list_y0 + 8, "..", Color::TEXT_SEC, 13.0);
    icons::draw(fb, "folder", list_x + 18, list_y0 + 6, 16, Color::TEXT_DIM);

    let (mut ndirs, mut nfiles, mut total) = (0usize, 0usize, 0u64);
    if let Some(l) = guard.as_ref() {
        let (d, f) = l.counts();
        ndirs = d;
        nfiles = f;
        total = l.total_size();
        for (i, e) in l.entries.iter().enumerate() {
            let ry = list_y0 + (i + 1) * ROW_H;
            if ry + ROW_H > ymax {
                break;
            }
            let is_dir = e.kind == FileKind::Dir;
            if SELECTED.load(Ordering::Relaxed) == i {
                fb.fill_rounded_rect(list_x + 6, ry + 1, list_w - 12, ROW_H - 4, 6, Color::ACCENT_SOFT);
            }
            let (glyph, gcol) = if is_dir { ("folder", accent) } else { ("doc", Color::TEXT_SEC) };
            icons::draw(fb, glyph, list_x + 18, ry + 6, 16, gcol);
            text::draw_px(fb, list_x + 44, ry + 8, &e.name, Color::INK, 13.0);
            // Badges (only really known ones, e.g. signed).
            let mut rx = list_x + list_w;
            if !is_dir {
                let sz = human_size(e.size);
                let sw = text::width_px(&sz, 11.5);
                text::draw_px(fb, list_x + list_w - sw - 16, ry + 9, &sz, Color::TEXT_DIM, 11.5);
                rx = list_x + list_w - sw - 28;
            }
            for b in &e.badges {
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
        text::draw_px(fb, list_x + 44, list_y0 + ROW_H + 8, "(directory loading…)", Color::TEXT_DIM, 12.5);
    }

    // ── Status bar ───────────────────────────────────────────────────────────
    let sy = by + bh - 26;
    fb.fill_rect(bx, sy, bw, 26, accent);
    text::draw_px(fb, bx + 14, sy + 6, "EuroFiles  ·  live EuroFS", Color::WHITE, 11.5);
    let right = alloc::format!("{} directories \u{00B7} {} files \u{00B7} {}", ndirs, nfiles, human_size(total));
    let rw = text::width_px(&right, 11.5);
    text::draw_px(fb, bx + bw - rw - 14, sy + 6, &right, Color::WHITE, 11.5);

    // ── Name-entry prompt (New Folder / Rename) ──────────────────────────────
    if let Some(p) = PROMPT.lock().as_ref() {
        let pw = 360usize;
        let ph = 96usize;
        let ox = bx + (bw - pw) / 2;
        let oy = by + (bh - ph) / 2;
        fb.fill_rounded_rect(ox - 2, oy - 2, pw + 4, ph + 4, 12, Color::rgb(0, 0, 0));
        fb.fill_rounded_rect(ox, oy, pw, ph, 10, Color::SURFACE);
        let title = if p.mode == PromptMode::Rename { "Rename to:" } else { "New folder name:" };
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
