//! The file open/save dialog: the picker an app shows to choose a file. It
//! browses the real filesystem (directories, files, up-navigation) and returns
//! the chosen path to the app. Wired into EuroText's Open button; reusable by
//! any app. Because filesystem access lives in the compositor loop, the dialog
//! holds the current directory and asks the loop to fill its entries.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use spin::Mutex;

use crate::graphics::{Color, FrameBuffer};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Open,
    Save,
}

struct State {
    mode: Mode,
    dir: String,
    entries: Vec<(String, bool)>, // (name, is_dir), directories first
    sel: usize,
    needs_load: bool,
    name: String, // Save: the filename being typed
}

static DLG: Mutex<Option<State>> = Mutex::new(None);
static RESULT: Mutex<Option<(Mode, String)>> = Mutex::new(None);

const W: usize = 540;
const H: usize = 440;
const ROW_H: usize = 28;
const LIST_ROWS: usize = 9;

pub fn is_open() -> bool {
    DLG.lock().is_some()
}

/// Open the dialog in `mode`, browsing `dir` (the loop will fill the entries).
pub fn open(mode: Mode, dir: &str) {
    *DLG.lock() = Some(State {
        mode,
        dir: dir.to_string(),
        entries: Vec::new(),
        sel: 0,
        needs_load: true,
        name: String::new(),
    });
}

pub fn close() {
    *DLG.lock() = None;
}

/// If the dialog needs a directory listed, return it (consumes the request).
pub fn needs_load() -> Option<String> {
    let mut g = DLG.lock();
    let st = g.as_mut()?;
    if st.needs_load {
        st.needs_load = false;
        Some(st.dir.clone())
    } else {
        None
    }
}

/// Fill the dialog's entries for `dir` (directories first, then files, sorted).
pub fn set_entries(dir: &str, mut items: Vec<(String, bool)>) {
    let mut g = DLG.lock();
    let Some(st) = g.as_mut() else { return };
    if st.dir != dir {
        return; // a newer navigation superseded this listing
    }
    items.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    st.entries = items;
    st.sel = 0;
}

fn parent(dir: &str) -> String {
    match dir.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(p) => dir[..p].to_string(),
    }
}

fn join(dir: &str, name: &str) -> String {
    if dir == "/" {
        alloc::format!("/{name}")
    } else {
        alloc::format!("{dir}/{name}")
    }
}

/// The full list including a ".." row when not at the root.
fn rows(st: &State) -> Vec<(String, bool, Option<String>)> {
    // (label, is_dir, navigate-target-or-None-for-file)
    let mut v = Vec::new();
    if st.dir != "/" {
        v.push(("..".to_string(), true, Some(parent(&st.dir))));
    }
    for (name, is_dir) in &st.entries {
        let path = join(&st.dir, name);
        v.push((name.clone(), *is_dir, if *is_dir { Some(path) } else { None }));
    }
    v
}

fn confirm(st: &mut State) -> Option<(Mode, String)> {
    match st.mode {
        Mode::Save => {
            if st.name.trim().is_empty() {
                return None;
            }
            Some((Mode::Save, join(&st.dir, st.name.trim())))
        }
        Mode::Open => {
            let r = rows(st);
            let (_, is_dir, nav) = r.get(st.sel)?.clone();
            if is_dir {
                None // navigation handled by caller; not a result
            } else {
                let _ = nav;
                Some((Mode::Open, join(&st.dir, &r[st.sel].0)))
            }
        }
    }
}

/// Feed a key. Handles typing a Save filename, arrow selection, Enter (open a
/// file / descend a directory / confirm a save) and Esc.
pub fn key(ch: char) {
    let mut g = DLG.lock();
    let Some(st) = g.as_mut() else { return };
    match ch {
        '\u{1b}' => {
            *g = None;
        }
        '\u{e}' => {
            let n = rows(st).len();
            if n > 0 { st.sel = (st.sel + 1) % n; }
        }
        '\u{10}' => {
            let n = rows(st).len();
            if n > 0 { st.sel = (st.sel + n - 1) % n; }
        }
        '\r' | '\n' => {
            // Descend into a selected directory first.
            let r = rows(st);
            if let Some((_, true, Some(target))) = r.get(st.sel).cloned() {
                if st.mode == Mode::Open || st.name.is_empty() {
                    st.dir = target;
                    st.needs_load = true;
                    return;
                }
            }
            if let Some(res) = confirm(st) {
                *RESULT.lock() = Some(res);
                *g = None;
            }
        }
        '\u{8}' | '\u{7f}' => {
            if st.mode == Mode::Save {
                st.name.pop();
            }
        }
        c if !c.is_control() => {
            if st.mode == Mode::Save {
                st.name.push(c);
            }
        }
        _ => {}
    }
}

fn geom(screen_w: usize, screen_h: usize) -> (usize, usize) {
    ((screen_w.saturating_sub(W)) / 2, (screen_h.saturating_sub(H)) / 2)
}

/// Handle a click. Returns true if it was consumed by the dialog.
pub fn click_at(mx: usize, my: usize, screen_w: usize, screen_h: usize) -> bool {
    let mut g = DLG.lock();
    let Some(st) = g.as_mut() else { return false };
    let (x, y) = geom(screen_w, screen_h);
    if mx < x || mx >= x + W || my < y || my >= y + H {
        *g = None; // click outside cancels
        return true;
    }
    // Buttons row (bottom).
    let by = y + H - 46;
    let confirm_rect = (x + W - 130, by, 110, 32);
    let cancel_rect = (x + W - 250, by, 110, 32);
    let hit = |r: (usize, usize, usize, usize)| mx >= r.0 && mx < r.0 + r.2 && my >= r.1 && my < r.1 + r.3;
    if hit(cancel_rect) {
        *g = None;
        return true;
    }
    if hit(confirm_rect) {
        if let Some(res) = confirm(st) {
            *RESULT.lock() = Some(res);
            *g = None;
        }
        return true;
    }
    // List rows.
    let list_top = y + 78;
    let r = rows(st);
    for (i, _) in r.iter().enumerate().take(LIST_ROWS) {
        let ry = list_top + i * ROW_H;
        if my >= ry && my < ry + ROW_H {
            st.sel = i;
            // A single click on a directory descends; on a file selects (Open on
            // the button or a second interaction confirms).
            if let (_, true, Some(target)) = r[i].clone() {
                st.dir = target;
                st.needs_load = true;
            }
            return true;
        }
    }
    true // clicks anywhere inside the card are consumed
}

/// Take a completed result (the chosen path), if any.
pub fn take_result() -> Option<(Mode, String)> {
    RESULT.lock().take()
}

pub fn render(fb: &FrameBuffer, screen_w: usize, screen_h: usize) {
    let g = DLG.lock();
    let Some(st) = g.as_ref() else { return };
    let (x, y) = geom(screen_w, screen_h);
    fb.fill_rounded_rect(x + 2, y + 4, W, H, crate::eds::RADIUS_L, Color::BORDER);
    fb.fill_rounded_rect(x, y, W, H, crate::eds::RADIUS_L, Color::CARD);
    fb.draw_border(x, y, W, H, 1, Color::BORDER);

    let title = if st.mode == Mode::Open { "Open file" } else { "Save file" };
    crate::text::draw_px(fb, x + 20, y + 18, title, Color::INK, 17.0);
    crate::text::draw_px(fb, x + 20, y + 48, &st.dir, Color::TEXT_DIM, 12.5);
    fb.fill_rect(x + 16, y + 70, W - 32, 1, Color::BORDER);

    let list_top = y + 78;
    let r = rows(st);
    for (i, (label, is_dir, _)) in r.iter().take(LIST_ROWS).enumerate() {
        let ry = list_top + i * ROW_H;
        if i == st.sel {
            fb.fill_rounded_rect(x + 8, ry + 1, W - 16, ROW_H - 2, crate::eds::RADIUS_S, Color::ACCENT_SOFT);
        }
        let icon = if *is_dir { "[dir] " } else { "      " };
        let c = if i == st.sel { Color::ACCENT } else { Color::INK };
        crate::text::draw_px(fb, x + 20, ry + 6, &alloc::format!("{icon}{label}"), c, 13.5);
    }

    // Save filename field.
    let by = y + H - 46;
    if st.mode == Mode::Save {
        let fy = by - 40;
        crate::text::draw_px(fb, x + 20, fy, "Name:", Color::TEXT_DIM, 12.0);
        fb.fill_rounded_rect(x + 74, fy - 6, 200, 26, crate::eds::RADIUS_S, Color::SURFACE);
        fb.draw_border(x + 74, fy - 6, 200, 26, 1, Color::BORDER);
        crate::text::draw_px(fb, x + 82, fy, &alloc::format!("{}|", st.name), Color::INK, 13.0);
    }

    // Buttons.
    let cancel = (x + W - 250, by, 110, 32);
    let confirm_r = (x + W - 130, by, 110, 32);
    fb.fill_rounded_rect(cancel.0, cancel.1, cancel.2, cancel.3, crate::eds::RADIUS_S, Color::SURFACE);
    fb.draw_border(cancel.0, cancel.1, cancel.2, cancel.3, 1, Color::BORDER);
    crate::text::draw_px(fb, cancel.0 + 32, cancel.1 + 8, "Cancel", Color::INK, 13.0);
    fb.fill_rounded_rect(confirm_r.0, confirm_r.1, confirm_r.2, confirm_r.3, crate::eds::RADIUS_S, Color::ACCENT);
    let label = if st.mode == Mode::Open { "Open" } else { "Save" };
    crate::text::draw_px(fb, confirm_r.0 + 38, confirm_r.1 + 8, label, Color::WHITE, 13.0);
}

/// `[fdlg]` boot self-test: the picker navigates and returns a chosen path in
/// both Open and Save modes.
pub fn selftest() {
    // Open mode: list a dir, select the file, Enter → that path. At "/" there is
    // no ".." row, so entries are [bin (dir), welcome.txt (file)]; one step down
    // lands on the file.
    open(Mode::Open, "/");
    let want_load = needs_load().as_deref() == Some("/");
    set_entries("/", alloc::vec![("bin".to_string(), true), ("welcome.txt".to_string(), false)]);
    key('\u{e}'); // sel: bin → welcome.txt
    key('\r'); // choose the file
    let open_ok = matches!(take_result(), Some((Mode::Open, p)) if p == "/welcome.txt");

    // Save mode: type a name, Enter → dir/name.
    open(Mode::Save, "/");
    let _ = needs_load();
    for c in "note.txt".chars() {
        key(c);
    }
    key('\r');
    let save_ok = matches!(take_result(), Some((Mode::Save, p)) if p == "/note.txt");

    let ok = want_load && open_ok && save_ok;
    crate::serial_println!(
        "[fdlg] File dialog: lists-a-dir={want_load}, open-returns-file={open_ok}, save-returns-path={save_ok} → {}",
        if ok { "OK (a real file picker for apps) ✓" } else { "FAILED ✗" }
    );
    close();
}
