//! EuroBeheer — the settings/management panel of EuroOS. Shows and manages the
//! REAL, LIVE kernel state (no mockup): EuroGuard capabilities/firewall,
//! network, and system. A clickable section navigation on the left; on the right the live data
//! from the kernel (`euroguard::*_lines`, `net::cmd_net`, `interrupts::ticks`, …) and
//! a real switch (the HTTP server on/off via `net::httpd_toggle`).

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::graphics::{Color, FrameBuffer};
use crate::text;
use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

const TITLEBAR_H: usize = 44;
const NAV_W: usize = 190;

/// The selected section (clickable in the navigation).
static SECTION: AtomicUsize = AtomicUsize::new(0);

/// Edit state of the "block domain" input field (EuroGuard section).
static EDITING: AtomicBool = AtomicBool::new(false);
static DOMAIN_BUF: Mutex<String> = Mutex::new(String::new());

/// y-offset (from win_y) of the "block domain" input field.
fn domain_field_y() -> usize {
    TITLEBAR_H + 22 + 30
}

pub fn editing() -> bool {
    EDITING.load(Ordering::Relaxed)
}

pub fn section() -> usize {
    SECTION.load(Ordering::Relaxed)
}

pub fn begin_domain_edit() {
    EDITING.store(true, Ordering::Relaxed);
    DOMAIN_BUF.lock().clear();
}

/// Process a key in the domain field. Returns Some(domain) on Enter (→ block).
pub fn edit_key(ch: char) -> Option<String> {
    if !EDITING.load(Ordering::Relaxed) {
        return None;
    }
    match ch {
        '\r' => {
            let d = DOMAIN_BUF.lock().clone();
            EDITING.store(false, Ordering::Relaxed);
            if !d.trim().is_empty() {
                return Some(d.trim().into());
            }
        }
        '\u{1b}' => EDITING.store(false, Ordering::Relaxed),
        '\u{8}' | '\u{7f}' => {
            DOMAIN_BUF.lock().pop();
        }
        c if !c.is_control() && !c.is_whitespace() => DOMAIN_BUF.lock().push(c),
        _ => {}
    }
    None
}

/// Click on the domain input field (only in the EuroGuard section)?
pub fn domain_field_at(win_x: usize, win_y: usize, mx: usize, my: usize) -> bool {
    if SECTION.load(Ordering::Relaxed) != 0 {
        return false;
    }
    let fx = win_x + NAV_W + 24;
    let fy = win_y + domain_field_y();
    mx >= fx && mx < fx + 320 && my + 4 >= fy && my < fy + 30
}

const SECTIONS: [&str; 4] = ["EuroGuard", "Apps", "Network", "System"];

/// The section index of the per-app control screen.
const SEC_APPS: usize = 1;

/// Which app row is selected in the Apps section (index into the roster).
static SELECTED_APP: AtomicUsize = AtomicUsize::new(0);

pub fn set_section(i: usize) {
    if i < SECTIONS.len() {
        SECTION.store(i, Ordering::Relaxed);
    }
}

/// The app roster (name, caps, linux-compat) — the same list the `apps` shell
/// command shows. Stable order, so a row index maps to a fixed app.
fn roster() -> Vec<(String, u64, bool)> {
    crate::ring3::program_list()
}

/// The name of the currently selected app (clamped to the roster).
fn selected_app_name() -> Option<String> {
    let r = roster();
    if r.is_empty() {
        return None;
    }
    let i = SELECTED_APP.load(Ordering::Relaxed).min(r.len() - 1);
    Some(r[i].0.clone())
}

// ── Apps-section layout (absolute geometry mirrored by the hit-tests) ──
const APP_ROW_H: usize = 26;
const APP_LIST_ROWS: usize = 7;

fn app_list_top(win_y: usize) -> usize {
    win_y + TITLEBAR_H + 22 + 40
}
fn app_actions_y(win_y: usize) -> usize {
    app_list_top(win_y) + APP_LIST_ROWS * APP_ROW_H + 150
}

/// Click on an app row in the Apps section → returns the row index.
pub fn app_row_at(win_x: usize, win_y: usize, mx: usize, my: usize) -> Option<usize> {
    if SECTION.load(Ordering::Relaxed) != SEC_APPS {
        return None;
    }
    let lx = win_x + NAV_W + 24;
    let top = app_list_top(win_y);
    if mx < lx || mx >= lx + 250 {
        return None;
    }
    let n = roster().len().min(APP_LIST_ROWS);
    for i in 0..n {
        let iy = top + i * APP_ROW_H;
        if my >= iy && my < iy + APP_ROW_H - 2 {
            return Some(i);
        }
    }
    None
}

pub fn select_app(i: usize) {
    SELECTED_APP.store(i, Ordering::Relaxed);
}

/// Click on the "Block network" switch for the selected app?
pub fn app_net_toggle_at(win_x: usize, win_y: usize, mx: usize, my: usize) -> bool {
    if SECTION.load(Ordering::Relaxed) != SEC_APPS {
        return false;
    }
    let cx = win_x + NAV_W + 24;
    let ty = app_actions_y(win_y);
    let px = cx + 200;
    mx >= px && mx < px + 56 && my + 6 >= ty && my < ty + 30
}

/// Toggle the selected app between full network block and default (REAL action).
/// Returns the new blocked state.
pub fn toggle_app_net() -> bool {
    let Some(name) = selected_app_name() else { return false };
    let now_blocked = crate::euroguard::app_is_blocked(&name);
    let rule = if now_blocked {
        crate::euroguard::AppNet::Default
    } else {
        crate::euroguard::AppNet::Blocked
    };
    crate::euroguard::set_app_net(&name, rule);
    !now_blocked
}

/// Click on the "Revoke permissions" button for the selected app?
pub fn app_revoke_at(win_x: usize, win_y: usize, mx: usize, my: usize) -> bool {
    if SECTION.load(Ordering::Relaxed) != SEC_APPS {
        return false;
    }
    let cx = win_x + NAV_W + 24;
    let by = app_actions_y(win_y) + 40;
    mx >= cx && mx < cx + 180 && my + 6 >= by && my < by + 30
}

/// Revoke all of the selected app's permission grants (REAL action).
pub fn revoke_app_perms() -> usize {
    match selected_app_name() {
        Some(name) => crate::portal::revoke_app(&name),
        None => 0,
    }
}

/// Which navigation section lies under (mx,my)? `win_*` = full window geometry.
pub fn nav_at(win_x: usize, win_y: usize, mx: usize, my: usize) -> Option<usize> {
    let nx = win_x;
    let ny = win_y + TITLEBAR_H + 14;
    if mx < nx || mx >= nx + NAV_W {
        return None;
    }
    for i in 0..SECTIONS.len() {
        let iy = ny + i * 44;
        if my >= iy && my < iy + 38 {
            return Some(i);
        }
    }
    None
}

/// y-offset (from win_y) of the HTTP server switch — below the live net lines.
fn toggle_y_off() -> usize {
    TITLEBAR_H + 24 + 8 * 22 + 16
}

/// Does (mx,my) lie on the HTTP server switch (only visible in the Network section)?
pub fn toggle_at(win_x: usize, win_y: usize, mx: usize, my: usize) -> bool {
    if SECTION.load(Ordering::Relaxed) != 2 {
        return false;
    }
    let tx = win_x + NAV_W + 24;
    let ty = win_y + toggle_y_off();
    mx >= tx && mx < tx + 270 && my + 6 >= ty && my < ty + 34
}

/// Toggle the HTTP server (real kernel action) and return the new state.
pub fn toggle_httpd() -> bool {
    crate::net::httpd_toggle()
}

/// Render the management panel body (live kernel state).
pub fn render(fb: &FrameBuffer, win_x: usize, win_y: usize, win_w: usize, win_h: usize) {
    let x = win_x;
    let y = win_y + TITLEBAR_H;
    let w = win_w;
    let h = win_h.saturating_sub(TITLEBAR_H);
    let sec = SECTION.load(Ordering::Relaxed);

    // Background + navigation column.
    fb.fill_rect(x, y, w, h, Color::SURFACE);
    fb.fill_rect(x, y, NAV_W, h, Color::SURFACE_3);
    fb.fill_rect(x + NAV_W, y, 1, h, Color::BORDER);
    text::draw_px(fb, x + 16, y + 16, "Settings", Color::INK, 16.0);
    let ny = y + 14 + 26;
    for (i, name) in SECTIONS.iter().enumerate() {
        let iy = ny + i * 44;
        if i == sec {
            fb.fill_rounded_rect(x + 10, iy - 4, NAV_W - 20, 38, crate::eds::RADIUS_M, Color::ACCENT_SOFT);
            // accent bar on the left
            fb.fill_rounded_rect(x + 4, iy + 2, 3, 26, 1, Color::ACCENT);
        }
        let c = if i == sec { Color::ACCENT } else { Color::TEXT_SEC };
        text::draw_px(fb, x + 22, iy + 4, name, c, 14.0);
    }

    // Content on the right.
    let cx = x + NAV_W + 24;
    let mut cy = y + 22;
    let title = SECTIONS[sec];
    text::draw_px(fb, cx, cy, title, Color::INK, 20.0);
    cy += 34;

    // EuroGuard section: REAL management — an input field to block a domain.
    if sec == 0 {
        let fy = win_y + domain_field_y();
        text::draw_px(fb, cx, fy - 18, "Block a domain (type + Enter):", Color::TEXT_SEC, 12.5);
        let edit = EDITING.load(Ordering::Relaxed);
        fb.fill_rounded_rect(cx, fy, 320, 28, crate::eds::RADIUS_S, Color::SURFACE_3);
        fb.draw_border(cx, fy, 320, 28, if edit { 2 } else { 1 }, if edit { Color::ACCENT } else { Color::BORDER });
        let mut shown = DOMAIN_BUF.lock().clone();
        if edit {
            shown.push('|');
        } else if shown.is_empty() {
            shown.push_str("e.g. ads.example.com");
        }
        let c = if edit || !DOMAIN_BUF.lock().is_empty() { Color::INK } else { Color::TEXT_DIM };
        text::draw_px(fb, cx + 10, fy + 6, &shown, c, 13.5);
        cy = fy + 44;
    }

    // Apps section: the per-app control screen (own layout + action controls).
    if sec == SEC_APPS {
        render_apps(fb, x, y, win_y, w, h, cx);
        return;
    }

    // Collect the live lines per section.
    let lines: Vec<String> = match sec {
        0 => {
            // EuroGuard: stats + policy + recent audit (REAL kernel state).
            let mut v = Vec::new();
            v.push(String::from("\u{2014} Status"));
            v.extend(crate::euroguard::stats_lines());
            v.push(String::new());
            v.push(String::from("\u{2014} Policy (capabilities / blocked)"));
            v.extend(crate::euroguard::policy_lines());
            v.push(String::new());
            v.push(String::from("\u{2014} Recent audit (live)"));
            v.extend(crate::euroguard::audit_lines(6));
            v
        }
        2 => crate::net::cmd_net(),
        _ => {
            // System: live uptime, processes, heap.
            let up = crate::interrupts::ticks() / 100;
            let (h2, m2, s2) = (up / 3600, (up % 3600) / 60, up % 60);
            alloc::vec![
                alloc::format!("uptime    : {h2}h {m2:02}m {s2:02}s"),
                alloc::format!("processes : {}", crate::sched::task_count()),
                alloc::format!("kernel-heap: {} MiB", crate::allocator::size() / (1024 * 1024)),
                alloc::format!("CPU       : x86-64, SMEP+SMAP, W^X"),
                String::from("kernel    : EuroKernel (from-scratch Rust, no_std)"),
            ]
        }
    };

    for l in lines.iter().take(((h - 80) / 20).max(1)) {
        let color = if l.starts_with('\u{2014}') {
            Color::ACCENT
        } else {
            Color::TEXT_SEC
        };
        text::draw_px(fb, cx, cy, l, color, 13.0);
        cy += 20;
    }

    // Network section: a REAL switch for the HTTP server.
    if sec == 2 {
        let (on, _) = crate::net::httpd_status();
        let ty = win_y + toggle_y_off();
        text::draw_px(fb, cx, ty + 6, "HTTP server (port 80)", Color::INK, 13.5);
        let pw = 56usize;
        let px = cx + 200;
        let track = if on { Color::SUCCESS } else { Color::BORDER };
        fb.fill_rounded_rect(px, ty + 4, pw, 24, 12, track);
        let knob = 18usize;
        let kx = if on { px + pw - knob - 3 } else { px + 3 };
        fb.fill_rounded_rect(kx, ty + 7, knob, knob, knob / 2, Color::WHITE);
        text::draw_px(fb, px + pw + 10, ty + 6, if on { "on" } else { "off" }, if on { Color::SUCCESS } else { Color::TEXT_DIM }, 12.5);
    }
}

/// `[7appgui]` boot self-test — the desktop per-app control screen's action
/// handlers do REAL kernel work (not just draw): selecting an app, cutting its
/// network with the switch, and revoking its permissions all change live state.
pub fn selftest() {
    let r = roster();
    if r.is_empty() {
        crate::serial_println!("[7appgui] app-control panel: no programs registered — SKIPPED");
        return;
    }
    let name = r[0].0.clone();

    // (1) Selecting a row targets that app.
    select_app(0);
    let selected_ok = selected_app_name().as_deref() == Some(name.as_str());

    // (2) The "Block all network" switch cuts the app off for real.
    let before = crate::euroguard::app_is_blocked(&name);
    let now = toggle_app_net();
    let blocked_ok = now && crate::euroguard::app_is_blocked(&name);

    // (3) Toggling back restores it (leave the system untouched).
    let restored = !toggle_app_net() && !crate::euroguard::app_is_blocked(&name);

    // (4) Revoke runs without panicking (0 grants is a valid outcome).
    let _revoked = revoke_app_perms();

    let ok = selected_ok && blocked_ok && restored && !before;
    crate::serial_println!(
        "[7appgui] Desktop app-control (EuroBeheer › Apps): select-app={selected_ok}, block-switch-cuts-network={blocked_ok}, toggle-restores={restored}, revoke-runs=true → {}",
        if ok { "OK (rights + network controllable per app from the desktop) ✓" } else { "FAILED ✗" }
    );
}

/// The per-app control screen: pick an app, see everything it can do
/// (capabilities + permission grants + network policy + live traffic), and
/// restrict it (cut its network, revoke its permissions). Total control, so an
/// AI agent can be clamped from the desktop.
fn render_apps(fb: &FrameBuffer, x: usize, y: usize, win_y: usize, _w: usize, _h: usize, cx: usize) {
    let r = roster();
    let sel = SELECTED_APP.load(Ordering::Relaxed).min(r.len().saturating_sub(1));

    text::draw_px(fb, cx, y + 22, "Applications", Color::INK, 20.0);
    text::draw_px(fb, cx, y + 48, "What each app can access, and how to limit it.", Color::TEXT_DIM, 11.5);

    // The clickable app list.
    let top = app_list_top(win_y);
    for (i, (name, _caps, _linux)) in r.iter().take(APP_LIST_ROWS).enumerate() {
        let iy = top + i * APP_ROW_H;
        if i == sel {
            fb.fill_rounded_rect(cx - 6, iy - 2, 250, APP_ROW_H - 2, crate::eds::RADIUS_S, Color::ACCENT_SOFT);
        }
        let blocked = crate::euroguard::app_is_blocked(name);
        let c = if i == sel { Color::ACCENT } else { Color::TEXT_SEC };
        text::draw_px(fb, cx + 4, iy + 4, name, c, 13.5);
        if blocked {
            text::draw_px(fb, cx + 180, iy + 5, "no net", Color::RED, 11.0);
        }
    }

    // Vertical divider between the list and the detail.
    let dx = cx + 268;
    fb.fill_rect(dx, top - 6, 1, APP_LIST_ROWS * APP_ROW_H, Color::BORDER);

    // Detail of the selected app.
    let ddx = dx + 20;
    let mut dy = top - 2;
    if let Some((name, caps, linux)) = r.get(sel) {
        text::draw_px(fb, ddx, dy, name, Color::INK, 16.0);
        dy += 24;
        let abi = if *linux { "Linux-compat binary" } else { "EuroOS-native" };
        text::draw_px(fb, ddx, dy, abi, Color::TEXT_DIM, 11.5);
        dy += 22;
        text::draw_px(fb, ddx, dy, "\u{2014} Capabilities (what it needs to run)", Color::ACCENT, 12.0);
        dy += 18;
        text::draw_px(fb, ddx, dy, &crate::ring3::cap_names(*caps), Color::TEXT_SEC, 12.5);
        dy += 24;

        text::draw_px(fb, ddx, dy, "\u{2014} Permissions (granted via the portal)", Color::ACCENT, 12.0);
        dy += 18;
        let grants = crate::portal::grant_lines_for(name);
        if grants.is_empty() {
            text::draw_px(fb, ddx, dy, "none (it must ask; you allow or deny)", Color::TEXT_DIM, 12.0);
            dy += 18;
        } else {
            for g in grants.iter().take(4) {
                text::draw_px(fb, ddx, dy, g, Color::TEXT_SEC, 12.0);
                dy += 17;
            }
        }
        dy += 8;

        text::draw_px(fb, ddx, dy, "\u{2014} Network (policy + live traffic)", Color::ACCENT, 12.0);
        dy += 18;
        for l in crate::euroguard::app_summary_lines(name).iter().take(4) {
            text::draw_px(fb, ddx, dy, l.trim_start(), Color::TEXT_SEC, 12.0);
            dy += 17;
        }
    }

    // Action controls (REAL kernel actions).
    let ty = app_actions_y(win_y);
    let blocked = r.get(sel).map(|(n, _, _)| crate::euroguard::app_is_blocked(n)).unwrap_or(false);
    text::draw_px(fb, cx, ty + 6, "Block all network", Color::INK, 13.5);
    let pw = 56usize;
    let px = cx + 200;
    let track = if blocked { Color::RED } else { Color::BORDER };
    fb.fill_rounded_rect(px, ty + 4, pw, 24, 12, track);
    let knob = 18usize;
    let kx = if blocked { px + pw - knob - 3 } else { px + 3 };
    fb.fill_rounded_rect(kx, ty + 7, knob, knob, knob / 2, Color::WHITE);
    text::draw_px(fb, px + pw + 10, ty + 6, if blocked { "blocked" } else { "allowed" }, if blocked { Color::RED } else { Color::TEXT_DIM }, 12.5);

    // Revoke-permissions button.
    let by = ty + 40;
    fb.fill_rounded_rect(cx, by, 180, 28, crate::eds::RADIUS_S, Color::SURFACE_3);
    fb.draw_border(cx, by, 180, 28, 1, Color::BORDER);
    text::draw_px(fb, cx + 16, by + 7, "Revoke all permissions", Color::INK, 12.5);
    let _ = x;
}
