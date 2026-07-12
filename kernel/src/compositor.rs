//! EuroDesktop compositor (Track 5).
//!
//! A software compositor that draws a desktop in the house style of the UI prototype:
//! dark background, left sidebar, overlapping windows with
//! title bars (traffic-light buttons) in z-order, and a mouse cursor on top.
//! Draws directly to the GOP framebuffer (software rendering); a
//! Vulkan backend is a later phase.

use alloc::string::String;
use alloc::vec::Vec;

use crate::eds::{self, SecState};
use crate::font::CHAR_HEIGHT;
use crate::graphics::{Color, FrameBuffer};

/// Left margin taken up by the floating dock (the dock itself is 62px @ x=14, + margin).
/// Windows begin to the right of this.
pub const SIDEBAR_W: usize = 90;
const TITLEBAR_H: usize = 44;
// Floating dock geometry (EDS: left/top/bottom 14, width 62).
const DOCK_X: usize = 14;
const DOCK_W: usize = 62;
const DOCK_M: usize = 14;
// Right status panel.
const PANEL_W: usize = 284;

// Dock tile metrics + the app order (index = `dock_targets` index in main.rs).
const TILE: usize = 42;
const TILE_GAP: usize = 8;
const TILE_TOP: usize = DOCK_M + 64;
/// The dock app tiles, from top to bottom. Honest mapping: each icon opens the
/// app it represents (AG-1 added files/notes/clock).
pub const DOCK_APPS: [&str; 11] =
    ["files", "notes", "clock", "browser", "terminal", "settings", "store", "star", "text", "monitor", "log"];
/// Which dock tile is opened/active (usize::MAX = none) — for the accent bar.
static ACTIVE_DOCK: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(usize::MAX);
/// Mark which dock tile is active (the kernel sets this on open/close).
pub fn set_active_dock(i: Option<usize>) {
    ACTIVE_DOCK.store(i.unwrap_or(usize::MAX), core::sync::atomic::Ordering::Relaxed);
}

/// A window (surface) with title, content, and security state (EDS). The body shows
/// `content` as monospace text lines (terminal / live system status) or, if `ui`
/// is not empty, a EuroUI widget panel.
pub struct Window {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
    pub title: String,
    pub content: Vec<String>,
    /// EuroUI widget panel; if not empty, drawn instead of `content` (text).
    pub ui: Vec<crate::euroui::Widget>,
    pub active: bool,
    pub accent: Color,
    pub sec: SecState,
    /// EuroSuite app (Writer/Calc/Impress) — drawn instead of text/widgets.
    pub app: crate::suite_ui::SuiteApp,
    /// Visible? `false` after closing/minimizing (not drawn, not clickable).
    pub visible: bool,
    /// Previous geometry (x,y,w,h) when the window is maximized; `None` = normal.
    pub restore: Option<(usize, usize, usize, usize)>,
}

/// Which title-bar button (traffic light) was clicked.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TitleButton {
    Close,
    Minimize,
    Maximize,
}

impl Window {
    pub fn titlebar_contains(&self, mx: usize, my: usize) -> bool {
        mx >= self.x && mx < self.x + self.w && my >= self.y && my < self.y + TITLEBAR_H
    }
    /// Is (mx,my) somewhere inside this window (for focus/raise on click)?
    pub fn contains(&self, mx: usize, my: usize) -> bool {
        mx >= self.x && mx < self.x + self.w && my >= self.y && my < self.y + self.h
    }
    /// Which traffic-light button is under (mx,my)? The three dots sit at
    /// x+14/34/54, vertically centered in the title bar, 13px — with a slightly
    /// more generous hit zone so they are easy to reach.
    pub fn title_button_at(&self, mx: usize, my: usize) -> Option<TitleButton> {
        let cy = self.y + (TITLEBAR_H - 13) / 2;
        // Vertically within the dot row (with margin)?
        if my + 3 < cy || my > cy + 16 {
            return None;
        }
        let mxi = mx as i32;
        for (i, base) in [14i32, 34, 54].into_iter().enumerate() {
            let cx = self.x as i32 + base + 6; // dot center
            if (mxi - cx).abs() <= 9 {
                return Some(match i {
                    0 => TitleButton::Close,
                    1 => TitleButton::Minimize,
                    _ => TitleButton::Maximize,
                });
            }
        }
        None
    }
}

/// Which dock tile index (see [`DOCK_APPS`]) is under (px,py)? None if the
/// click does not fall on a tile.
pub fn dock_icon_at(px: usize, py: usize) -> Option<usize> {
    if px < DOCK_X || px >= DOCK_X + DOCK_W {
        return None;
    }
    if py < TILE_TOP {
        return None;
    }
    let i = (py - TILE_TOP) / (TILE + TILE_GAP);
    if i < DOCK_APPS.len() && py < TILE_TOP + i * (TILE + TILE_GAP) + TILE {
        Some(i)
    } else {
        None
    }
}

/// The work-area rectangle (x,y,w,h) for a maximized window: between the
/// dock (left) and the status panel (right), with the EDS margin around it.
pub fn work_area(screen_w: usize, screen_h: usize) -> (usize, usize, usize, usize) {
    let x = SIDEBAR_W;
    let y = DOCK_M;
    let w = screen_w.saturating_sub(SIDEBAR_W + DOCK_M + PANEL_W + DOCK_M);
    let h = screen_h.saturating_sub(DOCK_M * 2);
    (x, y, w, h)
}

pub fn draw_window(fb: &FrameBuffer, win: &Window) {
    // Soft drop shadow — stronger for the active window (depth/EDS).
    let (spread, off) = if win.active { (16, 7) } else { (9, 4) };
    fb.drop_shadow(win.x, win.y, win.w, win.h, spread, off, Color::rgb(0x1A, 0x22, 0x2C));
    // Window body (EDS radius-L token).
    fb.fill_rounded_rect(win.x, win.y, win.w, win.h, eds::RADIUS_L, Color::SURFACE);

    // Title bar: surface-2, bottom straight so it aligns (EDS).
    let tb = Color::CARD;
    fb.fill_rounded_rect(win.x, win.y, win.w, TITLEBAR_H, eds::RADIUS_L, tb);
    fb.fill_rect(win.x, win.y + 18, win.w, TITLEBAR_H - 18, tb);

    // Traffic-light buttons (EDS colors).
    let cy = win.y + (TITLEBAR_H - 13) / 2;
    fb.fill_rounded_rect(win.x + 14, cy, 13, 13, 7, Color::rgb(0xEC, 0x6A, 0x5E));
    fb.fill_rounded_rect(win.x + 34, cy, 13, 13, 7, Color::rgb(0xF4, 0xBF, 0x50));
    fb.fill_rounded_rect(win.x + 54, cy, 13, 13, 7, Color::rgb(0x61, 0xC5, 0x54));

    // Title on the left after the buttons (icon accent + name), not centered.
    let ty = win.y + (TITLEBAR_H - 14) / 2;
    crate::text::draw_px(fb, win.x + 82, ty, &win.title, Color::INK, 13.0);

    // "Protected" pill on the right (green) — sandboxed & encrypted, EDS security.
    if win.sec.sandboxed {
        let label = "Protected";
        let lw = crate::text::width_px(label, 11.5);
        let pill_w = lw + 32;
        let pillx = win.x + win.w - 14 - pill_w;
        let pilly = win.y + (TITLEBAR_H - 22) / 2;
        fb.fill_rounded_rect(pillx, pilly, pill_w, 22, 11, Color::SUCCESS_SOFT);
        crate::icons::draw(fb, "shieldCheck", pillx + 7, pilly + 4, 14, Color::SUCCESS);
        crate::text::draw_px(fb, pillx + 25, pilly + 5, label, Color::SUCCESS, 11.5);
    }

    // Hairline under the title bar.
    fb.fill_rect(win.x, win.y + TITLEBAR_H, win.w, 1, Color::BORDER);

    // Inset gloss along the top edge (CSS: inset 0 .5px 0 rgba(255,255,255,.9)) —
    // a subtle white line just inside the rounded top edge that gives the glass
    // effect the reference has.
    let r = eds::RADIUS_L;
    let mut col = win.x + r;
    while col + 1 < win.x + win.w - r {
        fb.blend(col, win.y, Color::WHITE, 190);
        col += 1;
    }

    // Content (body) — separate so it can cheaply redraw only itself.
    draw_window_body(fb, win);
}

/// Redraw ONLY the body (content) of a window — for cheap live updates
/// (e.g. the System window per clock tick) without redrawing the drop shadow
/// (which would otherwise stack). First clears the old text and then draws the content.
pub fn draw_window_body(fb: &FrameBuffer, win: &Window) {
    // EuroReken: REAL calculator — render the LIVE state from `win.content`.
    if win.app == crate::suite_ui::SuiteApp::Reken {
        crate::calc_ui::render(fb, win.x, win.y, win.w, win.h, &win.content);
        return;
    }
    // EuroWeb: usable browser (tabs + address bar) — reads the global state.
    if win.app == crate::suite_ui::SuiteApp::Browser {
        crate::webview::render(fb, win.x, win.y, win.w, win.h);
        return;
    }
    // EuroBeheer: settings — shows the LIVE kernel state (euroguard/net/system).
    if win.app == crate::suite_ui::SuiteApp::Settings {
        crate::settings_ui::render(fb, win.x, win.y, win.w, win.h);
        return;
    }
    // EuroAgent: dispatch panel — intent + live cap-gated agent loop.
    if win.app == crate::suite_ui::SuiteApp::Agent {
        crate::agent_ui::render(fb, win.x, win.y, win.w, win.h);
        return;
    }
    // EuroInstall: guided graphical installer (plan + live FDE).
    if win.app == crate::suite_ui::SuiteApp::Installer {
        crate::installer::render(fb, win.x, win.y, win.w, win.h);
        return;
    }
    // EuroFiles: file manager — shows the LIVE EuroFS.
    if win.app == crate::suite_ui::SuiteApp::Files {
        crate::files::render(fb, win.x, win.y, win.w, win.h);
        return;
    }
    // EuroNotes: notes app — real Markdown via the euronotes engine.
    if win.app == crate::suite_ui::SuiteApp::Notes {
        crate::notes::render(fb, win.x, win.y, win.w, win.h);
        return;
    }
    // EuroClock: world clocks + local time from the REAL RTC.
    if win.app == crate::suite_ui::SuiteApp::Clock {
        crate::clockapp::render(fb, win.x, win.y, win.w, win.h);
        return;
    }
    if win.app == crate::suite_ui::SuiteApp::Text {
        crate::textedit::render(fb, win.x, win.y, win.w, win.h);
        return;
    }
    if win.app == crate::suite_ui::SuiteApp::Monitor {
        crate::monitor::render(fb, win.x, win.y, win.w, win.h);
        return;
    }
    if win.app == crate::suite_ui::SuiteApp::Log {
        crate::logview::render(fb, win.x, win.y, win.w, win.h);
        return;
    }
    // EuroSuite app? Render the rich Writer/Calc/Impress GUI.
    if win.app != crate::suite_ui::SuiteApp::None {
        crate::suite_ui::render(fb, win.x, win.y, win.w, win.h, win.app);
        return;
    }
    // Content: EuroUI widget panel or plain (monospace) text lines.
    if !win.ui.is_empty() {
        crate::euroui::draw_panel(fb, win.x, win.y + TITLEBAR_H, win.w, &win.ui);
        return;
    }
    // Clear the body background (old lines gone) — leave the bottom rounded corners
    // alone so they do not become square.
    let by = win.y + TITLEBAR_H + 1;
    let bh = win.h.saturating_sub(TITLEBAR_H + 1 + eds::RADIUS_L);
    fb.fill_rect(win.x + 1, by, win.w - 2, bh, Color::SURFACE);
    // Scroll to the BOTTOM: show the LAST lines (incl. the live prompt) instead of
    // the top of the buffer — that way you really see what the shell is doing now.
    let visible = (win.h.saturating_sub(TITLEBAR_H + 24)) / 16;
    let start = win.content.len().saturating_sub(visible);
    let mut ty = win.y + TITLEBAR_H + 12;
    for line in &win.content[start..] {
        if ty + CHAR_HEIGHT > win.y + win.h {
            break;
        }
        // Terminal/system status: monospace so columns line up.
        crate::text::draw_mono(fb, win.x + 16, ty, line, Color::TEXT_SEC, 1);
        ty += 16;
    }
}

/// The body rectangle area (x,y,w,h) of a window — for `present_rect` after a
/// `draw_window_body`.
pub fn window_body_rect(win: &Window) -> (usize, usize, usize, usize) {
    (win.x, win.y + TITLEBAR_H, win.w, win.h - TITLEBAR_H)
}

/// 12 EU stars in a ring (fixed offsets — no trig needed in no_std).
const STAR_RING: [(i8, i8); 12] = [
    (10, 0), (9, 5), (5, 9), (0, 10), (-5, 9), (-9, 5),
    (-10, 0), (-9, -5), (-5, -9), (0, -10), (5, -9), (9, -5),
];

/// The EU emblem: blue disc + ring of 12 golden stars, centered on
/// (cx,cy) with radius `r`.
fn draw_eu_mark(fb: &FrameBuffer, cx: usize, cy: usize, r: usize) {
    fb.fill_rounded_rect(cx - r, cy - r, r * 2, r * 2, r, Color::ACCENT);
    let star_r = (r as i32 * 13) / 18; // stars close to the edge
    for &(dx, dy) in &STAR_RING {
        let sx = cx as i32 + (dx as i32 * star_r) / 10;
        let sy = cy as i32 + (dy as i32 * star_r) / 10;
        fb.fill_rect((sx - 1) as usize, (sy - 1) as usize, 2, 2, Color::GOLD);
    }
}

/// Floating dock on the left: glass card with EU mark, colorful app tiles, active
/// bar, and user avatar at the bottom (EDS `v3-dock`).
fn draw_sidebar(fb: &FrameBuffer, h: usize) {
    let dh = h - DOCK_M * 2;
    // Card + soft shadow.
    fb.drop_shadow(DOCK_X, DOCK_M, DOCK_W, dh, 14, 6, Color::rgb(0x1A, 0x22, 0x2C));
    fb.fill_rounded_rect(DOCK_X, DOCK_M, DOCK_W, dh, eds::RADIUS_L, Color::SURFACE);
    fb.draw_border(DOCK_X, DOCK_M, DOCK_W, dh, 1, Color::BORDER);
    let cx = DOCK_X + DOCK_W / 2;

    // EU mark at the top (size 36 → r 18).
    draw_eu_mark(fb, cx, DOCK_M + 28, 18);
    fb.fill_rect(cx - 15, DOCK_M + 54, 30, 1, Color::BORDER);

    // Colorful app tiles.
    let tile = TILE;
    let tx = cx - tile / 2;
    let active = ACTIVE_DOCK.load(core::sync::atomic::Ordering::Relaxed);
    let mut iy = TILE_TOP;
    for (i, id) in DOCK_APPS.iter().enumerate() {
        crate::appicons::draw_tile(fb, tx, iy, tile, id);
        // Active/open: accent bar at the left edge of the dock.
        if i == active {
            fb.fill_rounded_rect(DOCK_X + 1, iy + tile / 2 - 6, 4, 12, 2, Color::ACCENT);
        }
        iy += tile + TILE_GAP;
    }

    // User avatar at the bottom: accent ring + initials of the LOGGED-IN user
    // (derived from the EuroID session — never hardcoded personal data).
    let av = 40usize;
    let ax = cx - av / 2;
    let ay = DOCK_M + dh - av - 12;
    fb.fill_rounded_rect(ax, ay, av, av, av / 2, Color::ACCENT); // 2px accent ring
    fb.fill_rounded_rect(ax + 2, ay + 2, av - 4, av - 4, (av - 4) / 2, Color::ACCENT_SOFT);
    let initials = crate::auth::session_initials();
    let iw = crate::text::width_px(&initials, 14.0);
    crate::text::draw_px(fb, ax + (av - iw) / 2, ay + 12, &initials, Color::ACCENT, 14.0);
}

/// Wallpaper in the "desktop.html" reference look, in software pixels:
/// 1) a cool→warm vertical gradient, 2) a soft EU-blue radial glow
/// center-left, 3) a subtle dotted grid. Replaces the flat `clear()`.
/// Cached wallpaper: the gradient + radial glow + dotted grid below cost a few
/// million float ops (sqrtf per glow block) — cheap on real hardware but seconds
/// of guest time under TCG, which made every full redraw (e.g. opening an app)
/// stall and trip the deadman watchdog. It only depends on width/height, so we
/// compute it once and restore it with a memcpy on every later frame.
static WALLPAPER_CACHE: spin::Mutex<alloc::vec::Vec<u32>> = spin::Mutex::new(alloc::vec::Vec::new());

fn draw_wallpaper(fb: &FrameBuffer) {
    {
        let cache = WALLPAPER_CACHE.lock();
        if cache.len() == fb.width() * fb.height() {
            fb.restore(&cache);
            return;
        }
    }
    // First time (or a resolution change): compute the expensive layer once.
    // draw_wallpaper runs before anything else in render(), so after this the
    // backbuffer holds only the wallpaper — snapshot it for reuse.
    draw_wallpaper_compute(fb);
    let mut cache = WALLPAPER_CACHE.lock();
    fb.snapshot(&mut cache);
}

fn draw_wallpaper_compute(fb: &FrameBuffer) {
    let w = fb.width();
    let h = fb.height().max(1);
    // Diagonal gradient: cool blue-grey (top-left) → warm sand-beige
    // (bottom-right). Slightly stronger contrast than the reference CSS so it is also
    // visible on a camera/screen dump, but still calm.
    let cool = Color::rgb(0xE6, 0xEC, 0xF4); // light blue-grey
    let warm = Color::rgb(0xED, 0xE4, 0xD4); // warm sand-beige
    let denom = (w + h).max(1) as f32;
    for row in 0..h {
        // Per row one base color on the diagonal (x=0); the horizontal drift is
        // small enough to approximate per row → 1 lerp + row fill (cheap).
        let t0 = row as f32 / denom;
        // Two segments so the horizontal component still tints along: left and
        // right halves colored slightly differently.
        let left = cool.lerp(warm, t0);
        let right = cool.lerp(warm, (row as f32 + w as f32 * 0.5) / denom);
        fb.fill_rect(0, row, w / 2, 1, left);
        fb.fill_rect(w / 2, row, w - w / 2, 1, right);
    }

    // 2) Soft radial glow (EU blue), center-left. Coarse: step 2px + 2×2 block.
    let gcx = (w / 6) as i32;
    let gcy = (h / 2) as i32;
    let gr = (w / 4).max(1) as i32;
    let grf = gr as f32;
    let mut py = (gcy - gr).max(0);
    let py_end = (gcy + gr).min(h as i32);
    while py < py_end {
        let mut px = (gcx - gr).max(0);
        let px_end = (gcx + gr).min(w as i32);
        while px < px_end {
            let dx = (px - gcx) as f32;
            let dy = (py - gcy) as f32;
            let d2 = dx * dx + dy * dy;
            if d2 < grf * grf {
                let t = 1.0 - crate::graphics::sqrtf(d2) / grf;
                let a = (30.0 * t * t) as u8; // ~12% at the heart, fading out smoothly
                if a > 0 {
                    let (ux, uy) = (px as usize, py as usize);
                    fb.blend(ux, uy, Color::ACCENT, a);
                    fb.blend(ux + 1, uy, Color::ACCENT, a);
                    fb.blend(ux, uy + 1, Color::ACCENT, a);
                    fb.blend(ux + 1, uy + 1, Color::ACCENT, a);
                }
            }
            px += 2;
        }
        py += 2;
    }

    // 3) Dotted grid (24px grid) — fine, visible texture. 2×2 dot so it
    //    stays legible after scaling; warm-dark tint, low opacity.
    let step = 24usize;
    let dot = Color::rgb(0x6B, 0x60, 0x52);
    let mut y = 8;
    while y < h {
        let mut x = 8;
        while x < w {
            fb.blend(x, y, dot, 34);
            fb.blend(x + 1, y, dot, 22);
            fb.blend(x, y + 1, dot, 22);
            x += step;
        }
        y += step;
    }
}

/// Live system figures for the status panel (real, changing values).
#[derive(Clone, Copy, Default)]
pub struct SysStats {
    pub free_mb: u64,
    pub total_mb: u64,
    pub uptime_s: u64,
    pub cores: u32,
    pub procs: u32,
}

/// Right status panel — the eye-catcher: real clock + "device safe" card +
/// a LIVE system card (RAM/uptime/cores/processes — changes while the OS runs).
/// `with_shadow=false` on tick updates so the shadow does not stack.
/// Bounding rectangle (x,y,w,h) of the status panel incl. shadow margin — for
/// DIRTY-RECT rendering: on a clock tick we blit only this area instead of the
/// whole screen (SPERF). Clock card (150) + gap (12) + system card (168) + margin.
pub fn status_panel_rect(w: usize) -> (usize, usize, usize, usize) {
    let px = w.saturating_sub(DOCK_M + PANEL_W);
    let py = DOCK_M;
    let total_h = 150 + 12 + 168 + 22; // panel content + shadow below
    (px.saturating_sub(2), py.saturating_sub(2), PANEL_W + 22, total_h)
}

pub fn draw_status_panel(fb: &FrameBuffer, w: usize, _h: usize, hm: &str, date: &str, stats: &SysStats, with_shadow: bool) {
    let px = w - DOCK_M - PANEL_W;
    let py = DOCK_M;
    let ch = 150usize; // height of the clock card
    if with_shadow {
        fb.drop_shadow(px, py, PANEL_W, ch, 12, 5, Color::rgb(0x1A, 0x22, 0x2C));
    }
    fb.fill_rounded_rect(px, py, PANEL_W, ch, eds::RADIUS_L, Color::SURFACE);
    fb.draw_border(px, py, PANEL_W, ch, 1, Color::BORDER);

    // Large clock (44px) + date (14px).
    crate::text::draw_px(fb, px + 18, py + 16, hm, Color::INK, 44.0);
    crate::text::draw_px(fb, px + 18, py + 66, date, Color::TEXT_SEC, 14.0);

    // Theme toggle (moon) top-right.
    let tb = 34usize;
    let tbx = px + PANEL_W - 16 - tb;
    let tby = py + 16;
    fb.fill_rounded_rect(tbx, tby, tb, tb, tb / 2, Color::BORDER); // round 1px border
    fb.fill_rounded_rect(tbx + 1, tby + 1, tb - 2, tb - 2, (tb - 2) / 2, Color::SURFACE);
    crate::icons::draw(fb, "moon", tbx + 8, tby + 8, 18, Color::TEXT_SEC);

    // Security-summary card — HONEST: measured boot + full-disk encryption are
    // sealed to a TPM, so we only assert them when a TPM is actually present.
    // Without one (e.g. this QEMU image) FDE is skipped, so we claim only the
    // capability sandbox, which is always on. Never show encryption that is off.
    let gy = py + 90;
    let gx = px + 14;
    let gw = PANEL_W - 28;
    let gh = 46usize;
    let tpm = crate::tpm::present();
    let (bg, fg, icon, head, sub): (Color, Color, &str, &str, &str) = if tpm {
        (Color::SUCCESS_SOFT, Color::SUCCESS, "shieldCheck", "Your device is safe",
         "Measured boot \u{00B7} encrypted \u{00B7} sandboxed")
    } else {
        (Color::SURFACE_3, Color::TEXT_SEC, "lock", "Capability-sandboxed",
         "No TPM \u{2014} full-disk encryption needs one")
    };
    fb.fill_rounded_rect(gx, gy, gw, gh, eds::RADIUS_M, bg);
    crate::icons::draw(fb, icon, gx + 12, gy + 12, 22, fg);
    crate::text::draw_px(fb, gx + 44, gy + 9, head, Color::INK, 13.0);
    crate::text::draw_px(fb, gx + 44, gy + 26, sub, Color::TEXT_SEC, 11.5);

    // ── Live system card (real, changing figures) ──
    let sy = py + ch + 12;
    let sh = 168usize;
    if with_shadow {
        fb.drop_shadow(px, sy, PANEL_W, sh, 12, 5, Color::rgb(0x1A, 0x22, 0x2C));
    }
    fb.fill_rounded_rect(px, sy, PANEL_W, sh, eds::RADIUS_L, Color::SURFACE);
    fb.draw_border(px, sy, PANEL_W, sh, 1, Color::BORDER);
    let lx = px + 18;
    crate::text::draw_px(fb, lx, sy + 14, "SYSTEM", Color::TEXT_DIM, 10.5);

    // Memory bar (used = total - free).
    let used = stats.total_mb.saturating_sub(stats.free_mb);
    crate::text::draw_px(fb, lx, sy + 34, "Memory", Color::TEXT_SEC, 13.0);
    let mr = alloc::format!("{} / {} MiB", used, stats.total_mb);
    let mrw = crate::text::width_px(&mr, 12.0);
    crate::text::draw_px(fb, px + PANEL_W - 18 - mrw, sy + 35, &mr, Color::INK, 12.0);
    let barw = PANEL_W - 36;
    let bary = sy + 54;
    fb.fill_rounded_rect(lx, bary, barw, 6, 3, Color::SURFACE_3);
    let frac = if stats.total_mb > 0 { (used * barw as u64 / stats.total_mb) as usize } else { 0 };
    if frac > 0 {
        fb.fill_rounded_rect(lx, bary, frac.max(3), 6, 3, Color::ACCENT);
    }

    // Text rows: uptime / cores / processes.
    let rows = [
        ("Uptime", fmt_uptime(stats.uptime_s)),
        ("CPU cores", alloc::format!("{} online", stats.cores)),
        ("Processes", alloc::format!("{}", stats.procs)),
    ];
    let mut ry = sy + 74;
    for (label, val) in rows {
        crate::text::draw_px(fb, lx, ry, label, Color::TEXT_SEC, 12.5);
        let vw = crate::text::width_px(&val, 12.5);
        crate::text::draw_px(fb, px + PANEL_W - 18 - vw, ry, &val, Color::INK, 12.5);
        ry += 22;
    }
}

fn fmt_uptime(s: u64) -> alloc::string::String {
    let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        alloc::format!("{}h {:02}m {:02}s", h, m, sec)
    } else if m > 0 {
        alloc::format!("{}m {:02}s", m, sec)
    } else {
        alloc::format!("{}s", sec)
    }
}

/// Classic arrow cursor (X=edge, .=fill, space=transparent).
const CURSOR: [&str; 16] = [
    "X          ",
    "XX         ",
    "X.X        ",
    "X..X       ",
    "X...X      ",
    "X....X     ",
    "X.....X    ",
    "X......X   ",
    "X.......X  ",
    "X........X ",
    "X.....XXXXX",
    "X..X..X    ",
    "X.X X..X   ",
    "XX  X..X   ",
    "X    X..X  ",
    "     XXX   ",
];

pub fn draw_cursor(fb: &FrameBuffer, mx: usize, my: usize) {
    for (row, line) in CURSOR.iter().enumerate() {
        for (col, ch) in line.bytes().enumerate() {
            let c = match ch {
                b'X' => Color::rgb(0x0A, 0x0F, 0x1A),
                b'.' => Color::WHITE,
                _ => continue,
            };
            fb.put_pixel(mx + col, my + row, c);
        }
    }
}

/// Draw the full desktop: warm background, floating dock, windows (z-order
/// `order`, back-to-front), and the right status panel on top.
/// The cursor is managed separately by the desktop loop.
pub fn render(fb: &FrameBuffer, windows: &[Window], order: &[usize], clock: &str, date: &str, stats: &SysStats) {
    let w = fb.width();
    let h = fb.height();
    draw_wallpaper(fb);
    draw_sidebar(fb, h);
    for &i in order {
        if windows[i].visible {
            draw_window(fb, &windows[i]);
        }
    }
    draw_status_panel(fb, w, h, clock, date, stats, true);
}

pub const CURSOR_W: usize = 11;
pub const CURSOR_H: usize = 16;

/// Save the pixels under the cursor (save-under) so we can erase it quickly.
pub fn save_cursor_bg(fb: &FrameBuffer, x: usize, y: usize, buf: &mut [Color]) {
    for r in 0..CURSOR_H {
        for c in 0..CURSOR_W {
            buf[r * CURSOR_W + c] = fb.get_pixel(x + c, y + r);
        }
    }
}

/// Restore the saved pixels (erase the cursor).
pub fn restore_cursor_bg(fb: &FrameBuffer, x: usize, y: usize, buf: &[Color]) {
    for r in 0..CURSOR_H {
        for c in 0..CURSOR_W {
            fb.put_pixel(x + c, y + r, buf[r * CURSOR_W + c]);
        }
    }
}
