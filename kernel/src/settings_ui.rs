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

const SECTIONS: [&str; 3] = ["EuroGuard", "Network", "System"];

pub fn set_section(i: usize) {
    if i < SECTIONS.len() {
        SECTION.store(i, Ordering::Relaxed);
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
    if SECTION.load(Ordering::Relaxed) != 1 {
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
        1 => crate::net::cmd_net(),
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
    if sec == 1 {
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
