//! Help: one page per app, "EuroOS at a glance", the security badges and the
//! keyboard shortcut overview, shown in an overlay panel (F1, F2, or the "?"
//! in a title bar). The text lives in `help_text.rs` as plain data (title +
//! sections), so it can be translated later without touching this file.
//!
//! The panel is an overlay like the launcher: it is drawn above the desktop
//! and takes mouse and keyboard while it is open. It draws with the existing
//! primitives (`text::draw_px`, EDS tokens), not a widget toolkit.

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use spin::Mutex;

use crate::compositor::Window;
use crate::graphics::{Color, FrameBuffer};
use crate::help_text::{Page, PAGES, SYSTEM_KEYS, SYSTEM_KEY_NOTES, POINTER_KEYS};
use crate::ps2::Key;
use crate::suite_ui::SuiteApp;
use crate::text;

/// Page ids that the code refers to by name.
pub const PAGE_OVERVIEW: &str = "overview";
pub const PAGE_SHORTCUTS: &str = "shortcuts";

struct State {
    page: usize,
    scroll: usize,
}

static HELP: Mutex<Option<State>> = Mutex::new(None);
static SCREEN_W: AtomicUsize = AtomicUsize::new(1920);
static SCREEN_H: AtomicUsize = AtomicUsize::new(1080);
static STARTED: AtomicBool = AtomicBool::new(false);
/// Repaint bookkeeping: the desktop loop's 2 s tick redraws parts of the
/// screen in place; the panel is repainted on the iteration after it.
static TICK_SLOT: AtomicU64 = AtomicU64::new(0);
static REPAINT: AtomicBool = AtomicBool::new(false);

const HEAD_H: usize = 46;
const FOOT_H: usize = 30;
const NAV_W: usize = 204;
const PAD: usize = 24;
const KEY_COL: usize = 168;

pub fn is_open() -> bool {
    HELP.lock().is_some()
}

pub fn close() {
    *HELP.lock() = None;
}

fn page_index(id: &str) -> Option<usize> {
    PAGES.iter().position(|p| p.id == id)
}

/// Open the panel on a page (by id); an unknown id opens the overview.
pub fn open_page(id: &str) {
    let page = page_index(id).or_else(|| page_index(PAGE_OVERVIEW)).unwrap_or(0);
    *HELP.lock() = Some(State { page, scroll: 0 });
}

/// The id of the page currently shown, if the panel is open.
pub fn current_page() -> Option<&'static str> {
    HELP.lock().as_ref().map(|s| PAGES[s.page].id)
}

/// The help page for a window: by app kind, and by title for the kinds that
/// host more than one thing (the Terminal and System windows are both plain
/// text windows; the hosted X window is Chromium or another Linux program).
pub fn page_for(app: SuiteApp, title: &str) -> &'static str {
    match app {
        SuiteApp::Files => "files",
        SuiteApp::Notes => "notes",
        SuiteApp::Clock => "clock",
        SuiteApp::Browser => "web",
        SuiteApp::Settings => "settings",
        SuiteApp::Reken => "calc",
        SuiteApp::Agent => "agent",
        SuiteApp::Text => "text",
        SuiteApp::Monitor => "monitor",
        SuiteApp::Log => "log",
        SuiteApp::ImageView => "view",
        SuiteApp::Paint => "paint",
        SuiteApp::Installer => "install",
        SuiteApp::Writer | SuiteApp::Calc | SuiteApp::Impress => "suite",
        SuiteApp::EuroSurface => "hosted",
        SuiteApp::XClient => {
            if title.contains("Chrom") || title.contains("chrom") { "chromium" } else { "hosted" }
        }
        SuiteApp::None => {
            if title.starts_with("Terminal") { "terminal" } else { PAGE_OVERVIEW }
        }
    }
}

/// The help page for a launcher / dock entry (index as in `dock_targets`).
pub fn page_for_icon(icon: usize) -> Option<&'static str> {
    const BY_ICON: [&str; 15] = [
        "files", "notes", "clock", "web", "terminal", "settings", "calc", "agent", "text", "monitor", "log",
        "chromium", "view", "paint", "install",
    ];
    if icon >= crate::launcher::APP_ICON_BASE {
        return Some("hosted");
    }
    BY_ICON.get(icon).copied()
}

/// Open help for a window (F1 on the focused window, or its "?" button).
pub fn open_for(win: &Window) {
    open_page(page_for(win.app, &win.title));
}

// ── Title-bar "?" button ─────────────────────────────────────────────────────

/// Rectangle of the "?" button in a window's title bar: left of the
/// "Protected" pill (or at the right edge when there is no pill). `None` for
/// a window too narrow to hold it next to the title.
pub fn title_button_rect(win: &Window) -> Option<(usize, usize, usize, usize)> {
    if win.w < 300 {
        return None;
    }
    let mut right = win.x + win.w - 14;
    if win.sec.sandboxed {
        // Same arithmetic as the pill in compositor::draw_window.
        let pill_w = text::width_px("Protected", 11.5) + 32;
        right = right.saturating_sub(pill_w + 8);
    }
    let size = 22;
    Some((right.saturating_sub(size), win.y + (crate::compositor::TITLEBAR_H - size) / 2, size, size))
}

pub fn title_button_at(win: &Window, mx: usize, my: usize) -> bool {
    match title_button_rect(win) {
        Some((x, y, w, h)) => mx >= x && mx < x + w && my >= y && my < y + h,
        None => false,
    }
}

/// Draw the "?" button (called from compositor::draw_window).
pub fn draw_title_button(fb: &FrameBuffer, win: &Window) {
    let Some((x, y, w, h)) = title_button_rect(win) else { return };
    fb.fill_rounded_rect(x, y, w, h, w / 2, Color::SURFACE_3);
    let qw = text::width_px_styled("?", 13.5, text::STYLE_BOLD, 0);
    text::draw_px_styled(fb, x + (w.saturating_sub(qw)) / 2, y + 4, "?", Color::TEXT_SEC, 13.5, text::STYLE_BOLD, 0);
}

// ── Desktop-loop hooks ───────────────────────────────────────────────────────

/// One call per desktop-loop iteration, before the loop's own input handling.
/// Handles F1/F2, the first-run tour, and (while the panel is open) the mouse
/// and keyboard. Returns true when the desktop must be repainted.
pub fn pump(windows: &[Window], order: &[usize], fs: &mut dyn eurofs::FileSystem, sw: usize, sh: usize) -> bool {
    SCREEN_W.store(sw, Ordering::Relaxed);
    SCREEN_H.store(sh, Ordering::Relaxed);
    if !STARTED.swap(true, Ordering::Relaxed) {
        // Function keys pressed before the desktop existed (lock screen) do not count.
        let _ = crate::ps2::take_help_key();
        let _ = crate::ps2::take_shortcuts_key();
    }
    // A full-screen program owns the display and the keyboard.
    if crate::appgfx::active() {
        let _ = crate::ps2::take_help_key();
        let _ = crate::ps2::take_shortcuts_key();
        return false;
    }
    let mut dirty = crate::onboarding::pump(fs, sw, sh);

    if crate::ps2::take_help_key() {
        crate::onboarding::dismiss();
        if is_open() {
            close();
        } else {
            // The window the user is working in: the active one, else the topmost.
            let target = order.iter().rev().copied().find(|&i| windows[i].visible && windows[i].active)
                .or_else(|| order.iter().rev().copied().find(|&i| windows[i].visible));
            match target {
                Some(i) => open_for(&windows[i]),
                None => open_page(PAGE_OVERVIEW),
            }
        }
        dirty = true;
    }
    if crate::ps2::take_shortcuts_key() {
        crate::onboarding::dismiss();
        if current_page() == Some(PAGE_SHORTCUTS) {
            close();
        } else {
            open_page(PAGE_SHORTCUTS);
        }
        dirty = true;
    }
    if is_open() {
        if let Some((cx, cy)) = crate::mouse::take_press() {
            click(cx, cy);
            dirty = true;
        }
        let _ = crate::mouse::take_right_press();
        while let Some(k) = crate::ps2::poll_key_ex() {
            key(k);
            dirty = true;
        }
    }
    if !is_open() && !crate::onboarding::is_active() {
        return dirty;
    }
    // Stay above the in-place redraws of the 2 s tick: repaint right after it.
    let slot = crate::interrupts::ticks() / 200;
    if TICK_SLOT.swap(slot, Ordering::Relaxed) != slot {
        REPAINT.store(true, Ordering::Relaxed);
    } else if REPAINT.swap(false, Ordering::Relaxed) {
        dirty = true;
    }
    dirty
}

/// Tooltip hook for the desktop loop. `Some(None)`: an overlay of this module
/// is up, show no tooltip. `Some(Some(text))`: the tooltip for a control this
/// module knows (the "?" button, a control inside an app). `None`: not ours.
pub fn tooltip_hook(px: usize, py: usize, windows: &[Window], order: &[usize]) -> Option<Option<String>> {
    if is_open() || crate::onboarding::is_active() {
        return Some(None);
    }
    if crate::compositor::brand_button_at(px, py) || crate::compositor::dock_icon_at(px, py).is_some() {
        return None;
    }
    let i = order.iter().rev().copied().find(|&i| windows[i].visible && windows[i].contains(px, py))?;
    let win = &windows[i];
    if title_button_at(win, px, py) {
        return Some(Some(String::from("Help for this window (F1)")));
    }
    if win.sec.sandboxed && win.titlebar_contains(px, py) {
        let pill_w = text::width_px("Protected", 11.5) + 32;
        if px + 14 + pill_w >= win.x + win.w && px + 14 < win.x + win.w {
            return Some(Some(String::from("Security label of this window: see Help, Security badges")));
        }
    }
    crate::tooltip::app_tip(win, px, py).map(|t| Some(String::from(t)))
}

// ── Layout ───────────────────────────────────────────────────────────────────

enum Row {
    Title(String),
    Summary(String),
    Head(String),
    /// A wrapped text line; `bullet` marks the first line of an item.
    Line(String, bool),
    Key(String, String),
    /// Continuation of a key description.
    KeyCont(String),
    Gap(usize),
}

fn row_h(r: &Row) -> usize {
    match r {
        Row::Title(_) => 34,
        Row::Summary(_) => 20,
        Row::Head(_) => 26,
        Row::Line(..) => 19,
        Row::Key(..) => 24,
        Row::KeyCont(_) => 18,
        Row::Gap(h) => *h,
    }
}

/// Greedy word wrap by pixel width.
pub fn wrap(s: &str, px: f32, max_w: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for word in s.split(' ') {
        if cur.is_empty() {
            cur.push_str(word);
            continue;
        }
        let mut cand = cur.clone();
        cand.push(' ');
        cand.push_str(word);
        if text::width_px(&cand, px) <= max_w {
            cur = cand;
        } else {
            out.push(core::mem::take(&mut cur));
            cur.push_str(word);
        }
    }
    if !cur.is_empty() || out.is_empty() {
        out.push(cur);
    }
    out
}

fn push_keys(rows: &mut Vec<Row>, keys: &[(&str, &str)], w: usize) {
    for (k, what) in keys {
        let mut lines = wrap(what, 13.0, w.saturating_sub(KEY_COL)).into_iter();
        rows.push(Row::Key(String::from(*k), lines.next().unwrap_or_default()));
        for l in lines {
            rows.push(Row::KeyCont(l));
        }
    }
}

fn push_lines(rows: &mut Vec<Row>, lines: &[&str], w: usize) {
    for item in lines {
        for (i, l) in wrap(item, 13.0, w.saturating_sub(16)).into_iter().enumerate() {
            rows.push(Row::Line(l, i == 0));
        }
        rows.push(Row::Gap(3));
    }
}

fn layout(page: &Page, w: usize) -> Vec<Row> {
    let mut rows = Vec::new();
    rows.push(Row::Title(String::from(page.title)));
    for l in wrap(page.summary, 13.5, w) {
        rows.push(Row::Summary(l));
    }
    rows.push(Row::Gap(6));
    if page.id == PAGE_SHORTCUTS {
        rows.push(Row::Head(String::from("Everywhere")));
        push_keys(&mut rows, SYSTEM_KEYS, w);
        rows.push(Row::Gap(4));
        push_lines(&mut rows, SYSTEM_KEY_NOTES, w);
        rows.push(Row::Head(String::from("Mouse")));
        push_keys(&mut rows, POINTER_KEYS, w);
        for p in PAGES.iter().filter(|p| !p.keys.is_empty()) {
            rows.push(Row::Gap(6));
            rows.push(Row::Head(String::from(p.nav)));
            push_keys(&mut rows, p.keys, w);
        }
        return rows;
    }
    for s in page.sections {
        rows.push(Row::Head(String::from(s.head)));
        push_lines(&mut rows, s.lines, w);
        rows.push(Row::Gap(4));
    }
    if !page.keys.is_empty() {
        rows.push(Row::Head(String::from("Keys")));
        push_keys(&mut rows, page.keys, w);
    }
    rows
}

struct Geom {
    x: usize,
    y: usize,
    w: usize,
    h: usize,
}

impl Geom {
    fn content_x(&self) -> usize {
        self.x + NAV_W + PAD
    }
    fn content_w(&self) -> usize {
        self.w.saturating_sub(NAV_W + PAD * 2 + 10)
    }
    fn content_y(&self) -> usize {
        self.y + HEAD_H + 14
    }
    fn content_h(&self) -> usize {
        self.h.saturating_sub(HEAD_H + FOOT_H + 22)
    }
    fn close_rect(&self) -> (usize, usize, usize, usize) {
        (self.x + self.w - 40, self.y + 10, 26, 26)
    }
    /// Nav list: (first row y, row height). A group label sits above the first
    /// app page and takes `NAV_LABEL_H`.
    fn nav(&self) -> (usize, usize) {
        let avail = self.h.saturating_sub(HEAD_H + FOOT_H + 16 + NAV_LABEL_H);
        let rh = (avail / PAGES.len().max(1)).clamp(15, 25);
        (self.y + HEAD_H + 8, rh)
    }
    fn nav_row_y(&self, i: usize) -> usize {
        let (y0, rh) = self.nav();
        let first_app = first_app_page();
        y0 + i * rh + if i >= first_app { NAV_LABEL_H } else { 0 }
    }
}

const NAV_LABEL_H: usize = 26;

fn first_app_page() -> usize {
    PAGES.iter().position(|p| p.app).unwrap_or(PAGES.len())
}

fn geom(sw: usize, sh: usize) -> Geom {
    let w = 960.min(sw.saturating_sub(40)).max(320);
    let h = 700.min(sh.saturating_sub(40)).max(240);
    Geom { x: sw.saturating_sub(w) / 2, y: sh.saturating_sub(h) / 2, w, h }
}

/// The largest scroll offset that still fills the content area.
fn max_scroll(rows: &[Row], avail: usize) -> usize {
    let mut h = 0;
    let mut first = rows.len();
    for (i, r) in rows.iter().enumerate().rev() {
        h += row_h(r);
        if h > avail {
            break;
        }
        first = i;
    }
    first
}

fn page_step(rows: &[Row], from: usize, avail: usize) -> usize {
    let mut h = 0;
    let mut n = 0usize;
    for r in rows.iter().skip(from) {
        h += row_h(r);
        if h > avail {
            break;
        }
        n += 1;
    }
    n.saturating_sub(2).max(1)
}

// ── Input ────────────────────────────────────────────────────────────────────

fn scroll_by(st: &mut State, down: bool, page: bool) {
    let g = geom(SCREEN_W.load(Ordering::Relaxed), SCREEN_H.load(Ordering::Relaxed));
    let rows = layout(&PAGES[st.page], g.content_w());
    let max = max_scroll(&rows, g.content_h());
    let step = if page { page_step(&rows, st.scroll, g.content_h()) } else { 2 };
    st.scroll = if down { (st.scroll + step).min(max) } else { st.scroll.saturating_sub(step) };
}

pub fn key(k: Key) {
    let mut g = HELP.lock();
    let Some(st) = g.as_mut() else { return };
    let n = PAGES.len();
    match k {
        Key::Esc => *g = None,
        Key::Down => scroll_by(st, true, false),
        Key::Up => scroll_by(st, false, false),
        Key::PageDown | Key::Char(' ') => scroll_by(st, true, true),
        Key::PageUp => scroll_by(st, false, true),
        Key::Home => st.scroll = 0,
        Key::End => {
            st.scroll = usize::MAX / 2;
            scroll_by(st, true, false);
        }
        Key::Right | Key::Tab => {
            st.page = (st.page + 1) % n;
            st.scroll = 0;
        }
        Key::Left => {
            st.page = (st.page + n - 1) % n;
            st.scroll = 0;
        }
        _ => {}
    }
}

pub fn click(mx: usize, my: usize) {
    let g = geom(SCREEN_W.load(Ordering::Relaxed), SCREEN_H.load(Ordering::Relaxed));
    let mut guard = HELP.lock();
    let Some(st) = guard.as_mut() else { return };
    // Outside the panel, or the close button: close.
    if mx < g.x || mx >= g.x + g.w || my < g.y || my >= g.y + g.h {
        *guard = None;
        return;
    }
    let (cx, cy, cw, ch) = g.close_rect();
    if mx >= cx && mx < cx + cw && my >= cy && my < cy + ch {
        *guard = None;
        return;
    }
    // Navigation list.
    if mx < g.x + NAV_W && my >= g.y + HEAD_H {
        let (_, rh) = g.nav();
        for i in 0..PAGES.len() {
            let ry = g.nav_row_y(i);
            if my >= ry && my < ry + rh {
                st.page = i;
                st.scroll = 0;
                return;
            }
        }
        return;
    }
    // Content: the upper third scrolls up, the rest scrolls down (also the
    // scroll track at the right edge).
    if my >= g.content_y() && my < g.content_y() + g.content_h() {
        let down = my >= g.content_y() + g.content_h() / 3;
        scroll_by(st, down, true);
    }
}

// ── Render ───────────────────────────────────────────────────────────────────

/// Draw the overlays of this module and of the tour (called from the desktop
/// loop's full repaint, above the windows and the other overlays).
pub fn render(fb: &FrameBuffer, sw: usize, sh: usize) {
    crate::onboarding::render(fb, sw, sh);
    render_panel(fb, sw, sh);
}

fn render_panel(fb: &FrameBuffer, sw: usize, sh: usize) {
    let guard = HELP.lock();
    let Some(st) = guard.as_ref() else { return };
    let g = geom(sw, sh);
    let page = &PAGES[st.page];

    fb.drop_shadow(g.x, g.y, g.w, g.h, 22, 10, Color::rgb(0x1A, 0x22, 0x2C));
    fb.fill_rounded_rect(g.x, g.y, g.w, g.h, crate::eds::RADIUS_L, Color::SURFACE);
    fb.draw_border(g.x, g.y, g.w, g.h, 1, Color::BORDER);

    // Header.
    fb.fill_rounded_rect(g.x, g.y, g.w, HEAD_H, crate::eds::RADIUS_L, Color::CARD);
    fb.fill_rect(g.x, g.y + 20, g.w, HEAD_H - 20, Color::CARD);
    fb.fill_rect(g.x, g.y + HEAD_H, g.w, 1, Color::BORDER);
    text::draw_px_styled(fb, g.x + 20, g.y + 14, "Help", Color::INK, 15.5, text::STYLE_BOLD, 0);
    text::draw_px(fb, g.x + 66, g.y + 16, "EuroOS preview", Color::TEXT_DIM, 12.0);
    let (cx, cy, cw, ch) = g.close_rect();
    fb.fill_rounded_rect(cx, cy, cw, ch, cw / 2, Color::SURFACE_3);
    let xw = text::width_px("x", 14.0);
    text::draw_px(fb, cx + (cw - xw) / 2, cy + 4, "x", Color::TEXT_SEC, 14.0);

    // Navigation.
    fb.fill_rounded_rect(g.x + 1, g.y + g.h - 44, NAV_W - 1, 43, crate::eds::RADIUS_L - 1, Color::CARD);
    fb.fill_rect(g.x + NAV_W / 2, g.y + g.h - 44, NAV_W - NAV_W / 2, 43, Color::CARD);
    fb.fill_rect(g.x + 1, g.y + HEAD_H + 1, NAV_W - 1, g.h - HEAD_H - 30, Color::CARD);
    fb.fill_rect(g.x + NAV_W, g.y + HEAD_H + 1, 1, g.h - HEAD_H - 2, Color::BORDER);
    let (_, rh) = g.nav();
    let npx = if rh >= 21 { 12.5 } else { 11.0 };
    let first_app = first_app_page();
    for (i, p) in PAGES.iter().enumerate() {
        let ry = g.nav_row_y(i);
        if i == first_app {
            text::draw_px(fb, g.x + 20, ry - NAV_LABEL_H + 9, "APPS", Color::TEXT_DIM, 10.5);
        }
        if i == st.page {
            fb.fill_rounded_rect(g.x + 10, ry, NAV_W - 20, rh.saturating_sub(2), 7, Color::ACCENT_SOFT);
        }
        let c = if i == st.page { Color::ACCENT } else { Color::INK };
        let ty = ry + rh.saturating_sub(text::line_height(npx)) / 2;
        text::draw_px(fb, g.x + 20, ty.saturating_sub(1), p.nav, c, npx);
    }

    // Content.
    let rows = layout(page, g.content_w());
    let avail = g.content_h();
    let max = max_scroll(&rows, avail);
    let scroll = st.scroll.min(max);
    let x = g.content_x();
    let mut y = g.content_y();
    let bottom = g.content_y() + avail;
    let mut shown_to = scroll;
    for (i, r) in rows.iter().enumerate().skip(scroll) {
        let h = row_h(r);
        if y + h > bottom {
            break;
        }
        match r {
            Row::Title(s) => text::draw_px_styled(fb, x, y, s, Color::INK, 21.0, text::STYLE_BOLD, 0),
            Row::Summary(s) => text::draw_px(fb, x, y, s, Color::TEXT_SEC, 13.5),
            Row::Head(s) => text::draw_px_styled(fb, x, y + 6, s, Color::INK, 13.5, text::STYLE_BOLD, 0),
            Row::Line(s, bullet) => {
                if *bullet {
                    fb.fill_rounded_rect(x + 3, y + 7, 4, 4, 2, Color::ACCENT);
                }
                text::draw_px(fb, x + 16, y, s, Color::INK, 13.0);
            }
            Row::Key(k, what) => {
                let kw = text::width_px_styled(k, 12.0, text::STYLE_BOLD, 0) + 16;
                fb.fill_rounded_rect(x, y + 1, kw.min(KEY_COL - 8), 20, 6, Color::SURFACE_3);
                text::draw_px_styled(fb, x + 8, y + 4, k, Color::INK, 12.0, text::STYLE_BOLD, 0);
                text::draw_px(fb, x + KEY_COL, y + 3, what, Color::INK, 13.0);
            }
            Row::KeyCont(what) => text::draw_px(fb, x + KEY_COL, y, what, Color::INK, 13.0),
            Row::Gap(_) => {}
        }
        y += h;
        shown_to = i + 1;
    }

    // Scroll track (only when the page does not fit).
    if max > 0 {
        let tx = g.x + g.w - 16;
        let ty = g.content_y();
        fb.fill_rounded_rect(tx, ty, 6, avail, 3, Color::SURFACE_3);
        let total = rows.len().max(1);
        let th = (avail * (shown_to - scroll) / total).max(24).min(avail);
        let tpos = if max == 0 { 0 } else { (avail - th) * scroll / max };
        fb.fill_rounded_rect(tx, ty + tpos, 6, th, 3, Color::TEXT_DIM);
    }

    // Footer.
    let fy = g.y + g.h - FOOT_H;
    fb.fill_rect(g.x + NAV_W + 1, fy, g.w - NAV_W - 2, 1, Color::BORDER);
    let hint = if max > 0 {
        if scroll < max {
            "More below: PgDn or click the text.   Esc closes.   Left/Right: other page.   F2: all shortcuts."
        } else {
            "End of page: PgUp goes back.   Esc closes.   Left/Right: other page.   F2: all shortcuts."
        }
    } else {
        "Esc closes.   Left/Right: other page.   F2: all shortcuts."
    };
    let hint_w = g.w.saturating_sub(NAV_W + PAD * 2);
    let hint = if text::width_px(hint, 11.5) <= hint_w { hint } else { "Esc closes.   PgUp/PgDn scroll.   F2: shortcuts." };
    text::draw_px(fb, x, fy + 9, hint, Color::TEXT_DIM, 11.5);
}

// ── Shell: `help`, `help <command>`, `help tour` ─────────────────────────────

/// `help` (grouped command list) and `help <command>` (one line + example).
pub fn shell_help(arg: &str) -> Vec<String> {
    use crate::help_text::{CMDS, CMD_GROUPS};
    let arg = arg.trim();
    if arg.is_empty() {
        let mut out = alloc::vec![
            String::from("EuroShell commands by topic.  help <command> = what it does + an example."),
            String::from("F1 = help window for the active app,  F2 = keyboard shortcuts,  help tour = the tour."),
        ];
        for (gi, gname) in CMD_GROUPS.iter().enumerate() {
            let mut line = alloc::format!("{:<10}", gname);
            let mut first = true;
            for c in CMDS.iter().filter(|c| c.group as usize == gi) {
                if line.len() + c.name.len() + 1 > 78 {
                    out.push(core::mem::take(&mut line));
                    line = alloc::format!("{:<10}", "");
                } else if !first {
                    line.push(' ');
                }
                line.push_str(c.name);
                first = false;
            }
            out.push(line);
        }
        out.push(String::from("Pipes and redirection: a | b    cmd > file    cmd >> file"));
        return out;
    }
    if arg == "tour" {
        crate::onboarding::restart();
        return alloc::vec![String::from("The tour is shown on the desktop (Esc closes it).")];
    }
    match CMDS.iter().find(|c| c.name == arg || c.alias.split(' ').any(|a| !a.is_empty() && a == arg)) {
        Some(c) => {
            let mut out = alloc::vec![alloc::format!("{}  -  {}", c.usage, c.what)];
            if !c.example.is_empty() {
                out.push(alloc::format!("example: {}", c.example));
            }
            if !c.alias.is_empty() {
                out.push(alloc::format!("also: {}", c.alias));
            }
            out
        }
        None => alloc::vec![alloc::format!("help: no entry for '{arg}'.  Type help for the list by topic.")],
    }
}

/// Terminal hook: the help output for a command line, if it is a help command.
pub fn shell_line(line: &str) -> Option<Vec<String>> {
    let line = line.trim();
    if line == "help" {
        return Some(shell_help(""));
    }
    line.strip_prefix("help ").map(shell_help)
}

// ── Boot self-test ───────────────────────────────────────────────────────────

/// `[help]` boot self-test: every app (window kind, launcher entry, dock tile)
/// has a help page with content, the shortcut page is generated, the command
/// table is consistent, and the in-app tooltip table answers.
pub fn selftest() {
    use crate::help_text::{CMDS, CMD_GROUPS};
    const KINDS: [(SuiteApp, &str); 21] = [
        (SuiteApp::None, "Terminal  -  /bin/sh"),
        (SuiteApp::None, "System"),
        (SuiteApp::Writer, ""),
        (SuiteApp::Calc, ""),
        (SuiteApp::Impress, ""),
        (SuiteApp::Browser, ""),
        (SuiteApp::Reken, ""),
        (SuiteApp::Settings, ""),
        (SuiteApp::Agent, ""),
        (SuiteApp::Installer, ""),
        (SuiteApp::Files, ""),
        (SuiteApp::Notes, ""),
        (SuiteApp::Clock, ""),
        (SuiteApp::Text, ""),
        (SuiteApp::ImageView, ""),
        (SuiteApp::Paint, ""),
        (SuiteApp::Monitor, ""),
        (SuiteApp::Log, ""),
        (SuiteApp::XClient, "Chromium"),
        (SuiteApp::XClient, "EuroOS GTK  -  ggtk"),
        (SuiteApp::EuroSurface, "EuroApp"),
    ];
    let has_content = |id: &str| {
        page_index(id)
            .map(|i| &PAGES[i])
            .map(|p| !p.title.is_empty() && !p.summary.is_empty() && (p.id == PAGE_SHORTCUTS || (!p.sections.is_empty() && p.sections.iter().all(|s| !s.lines.is_empty()))))
            .unwrap_or(false)
    };
    let kinds_ok = KINDS.iter().filter(|(a, t)| has_content(page_for(*a, t))).count();
    // Every launcher entry (dock tiles 0..=10 and the launcher-only apps).
    let mut icons = 0;
    let mut icons_ok = 0;
    for icon in 0..15 {
        if crate::launcher::name_for_icon(icon).is_some() {
            icons += 1;
            if page_for_icon(icon).map(has_content).unwrap_or(false) {
                icons_ok += 1;
            }
        }
    }
    let app_pages = PAGES.iter().filter(|p| p.app).count();
    let unique_ids = PAGES.iter().enumerate().all(|(i, p)| PAGES.iter().skip(i + 1).all(|q| q.id != p.id));
    // The shortcut overview lists the system keys and every app that has keys.
    let sc = page_index(PAGE_SHORTCUTS).map(|i| layout(&PAGES[i], 600)).unwrap_or_default();
    let sc_keys = sc.iter().filter(|r| matches!(r, Row::Key(..))).count();
    let sc_ok = sc_keys >= SYSTEM_KEYS.len() + POINTER_KEYS.len() + PAGES.iter().map(|p| p.keys.len()).sum::<usize>();
    // Panel state machine: open, page change, close.
    open_page("paint");
    let opened = current_page() == Some("paint");
    key(Key::Right);
    let moved = is_open() && current_page() != Some("paint");
    key(Key::Esc);
    let closed = !is_open();
    // Shell help: grouped list, one command, unknown command; unique names.
    let cmds_unique = CMDS.iter().enumerate().all(|(i, c)| (c.group as usize) < CMD_GROUPS.len() && CMDS.iter().skip(i + 1).all(|d| d.name != c.name));
    let sh_list = shell_help("");
    let sh_ok = sh_list.len() >= CMD_GROUPS.len() + 2
        && sh_list.iter().all(|l| l.len() <= 96)
        && shell_help("ls").iter().any(|l| l.starts_with("example:"))
        && shell_help("wget").first().map(|l| l.starts_with("fetch")).unwrap_or(false)
        && shell_help("nosuchcmd").len() == 1
        && shell_line("helpx").is_none()
        && shell_line("help ls").is_some();
    let tips = crate::tooltip::app_tips_selftest();
    let tour = crate::onboarding::selftest();
    let ok = kinds_ok == KINDS.len() && icons_ok == icons && icons == 15 && unique_ids && sc_ok && opened && moved && closed && cmds_unique && sh_ok && tips && tour;
    crate::serial_println!(
        "[help] help pages: window kinds {kinds_ok}/{} · launcher apps {icons_ok}/{icons} · {} pages ({app_pages} apps) · shortcut rows {sc_keys} · panel open/page/close={opened}/{moved}/{closed} · shell help {} cmds ok={sh_ok} · in-app tooltips={tips} · tour steps ok={tour} → {}",
        KINDS.len(),
        PAGES.len(),
        CMDS.len(),
        if ok { "OK (every app has a help page; F1, F2, ?) ✓" } else { "FAILED ✗" }
    );
}
