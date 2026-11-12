//! Notifications: transient toasts plus a notification centre (history shade).
//! Any part of the system can `push` a message; it appears as a toast top-right
//! for a few seconds and is kept in the centre, which the user opens by clicking
//! the status panel and closes with Esc, a click beside it or a second click on
//! the status panel. This is the "something happened" channel a desktop needs.
//!
//! Neither the toasts nor the list change between two events, so the desktop
//! loop repaints for them only when [`generation`], [`active_toasts`] or
//! [`is_centre_open`] changes (BUG-057), not in every iteration.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use spin::Mutex;

use crate::graphics::{Color, FrameBuffer};

pub struct Note {
    pub title: String,
    pub body: String,
    pub tick: u64,
}

static NOTES: Mutex<Vec<Note>> = Mutex::new(Vec::new());
static CENTRE_OPEN: AtomicBool = AtomicBool::new(false);
/// Counts every `push`: the list and the toasts show something new.
static GENERATION: AtomicU32 = AtomicU32::new(0);

const CAP: usize = 50;
/// How long a toast stays on screen (ticks; the loop runs ~100 Hz).
const TOAST_TICKS: u64 = 320;
const TOAST_W: usize = 320;
const CENTRE_W: usize = 340;
const CENTRE_Y: usize = 120;

/// Post a notification (title + one-line body) stamped with the current tick.
pub fn push(title: &str, body: &str, now: u64) {
    let mut n = NOTES.lock();
    n.push(Note { title: title.to_string(), body: body.to_string(), tick: now });
    let len = n.len();
    if len > CAP {
        n.drain(0..len - CAP);
    }
    GENERATION.fetch_add(1, Ordering::Relaxed);
    crate::serial_println!("[notify] {title}: {body}");
}

pub fn is_centre_open() -> bool {
    CENTRE_OPEN.load(Ordering::Relaxed)
}

pub fn toggle_centre() {
    let v = !CENTRE_OPEN.load(Ordering::Relaxed);
    CENTRE_OPEN.store(v, Ordering::Relaxed);
}

pub fn close_centre() {
    CENTRE_OPEN.store(false, Ordering::Relaxed);
}

/// Are any toasts still within their on-screen window?
pub fn has_active_toasts(now: u64) -> bool {
    NOTES.lock().iter().any(|n| now.saturating_sub(n.tick) < TOAST_TICKS)
}

/// How many toasts are on screen at `now`. It changes when one expires.
pub fn active_toasts(now: u64) -> usize {
    NOTES.lock().iter().filter(|n| now.saturating_sub(n.tick) < TOAST_TICKS).count()
}

/// Number of notifications posted since boot (wraps): changes with every `push`.
pub fn generation() -> u32 {
    GENERATION.load(Ordering::Relaxed)
}

/// The rectangle (x, y, w, h) of the notification list on a screen of this size.
pub fn centre_rect(screen_w: usize, screen_h: usize) -> (usize, usize, usize, usize) {
    let w = CENTRE_W;
    let y = CENTRE_Y;
    (screen_w.saturating_sub(w + 16), y, w, screen_h.saturating_sub(y + 40))
}

/// Is (mx, my) on the open notification list?
pub fn centre_contains(mx: usize, my: usize, screen_w: usize, screen_h: usize) -> bool {
    let (x, y, w, h) = centre_rect(screen_w, screen_h);
    is_centre_open() && mx >= x && mx < x + w && my >= y && my < y + h
}

/// Number of notifications in the centre.
pub fn count() -> usize {
    NOTES.lock().len()
}

/// Recent history lines for the `notify` shell command.
pub fn history_lines() -> Vec<String> {
    NOTES.lock().iter().rev().map(|n| alloc::format!("{} — {}", n.title, n.body)).collect()
}

/// Draw the live toasts, stacked top-right under the status panel.
pub fn render_toasts(fb: &FrameBuffer, screen_w: usize, now: u64) {
    let x = screen_w.saturating_sub(TOAST_W + 20);
    let mut y = 150usize;
    let notes = NOTES.lock();
    for n in notes.iter().rev() {
        if now.saturating_sub(n.tick) >= TOAST_TICKS {
            continue;
        }
        let h = 58usize;
        fb.fill_rounded_rect(x + 1, y + 3, TOAST_W, h, crate::eds::RADIUS_M, Color::BORDER);
        fb.fill_rounded_rect(x, y, TOAST_W, h, crate::eds::RADIUS_M, Color::CARD);
        fb.draw_border(x, y, TOAST_W, h, 1, Color::BORDER);
        fb.fill_rounded_rect(x, y, 4, h, 2, Color::ACCENT);
        crate::text::draw_px(fb, x + 16, y + 11, &n.title, Color::INK, 14.0);
        crate::text::draw_px(fb, x + 16, y + 32, &n.body, Color::TEXT_SEC, 12.0);
        y += h + 10;
        if y > 600 {
            break;
        }
    }
}

/// Draw the notification centre (history shade) when open.
pub fn render_centre(fb: &FrameBuffer, screen_w: usize, screen_h: usize) {
    if !is_centre_open() {
        return;
    }
    let (x, y, w, h) = centre_rect(screen_w, screen_h);
    fb.fill_rounded_rect(x + 1, y + 3, w, h, crate::eds::RADIUS_L, Color::BORDER);
    fb.fill_rounded_rect(x, y, w, h, crate::eds::RADIUS_L, Color::CARD);
    fb.draw_border(x, y, w, h, 1, Color::BORDER);
    crate::text::draw_px(fb, x + 20, y + 18, "Notifications", Color::INK, 16.0);
    let notes = NOTES.lock();
    if notes.is_empty() {
        crate::text::draw_px(fb, x + 20, y + 54, "Nothing yet", Color::TEXT_DIM, 13.0);
        return;
    }
    let mut cy = y + 52;
    for n in notes.iter().rev() {
        if cy + 46 > y + h {
            break;
        }
        fb.fill_rounded_rect(x + 12, cy, w - 24, 44, crate::eds::RADIUS_S, Color::SURFACE);
        crate::text::draw_px(fb, x + 22, cy + 7, &n.title, Color::INK, 13.0);
        crate::text::draw_px(fb, x + 22, cy + 25, &n.body, Color::TEXT_SEC, 11.5);
        cy += 52;
    }
}

/// `[notif]` boot self-test: a posted notification lands in the centre, shows as
/// an active toast within its window, and the toast expires while the history
/// is kept; the centre toggles.
pub fn selftest() {
    let before = count();
    push("Test", "hello", 1000);
    let stored = count() == before + 1;
    let toast_now = has_active_toasts(1000);
    let toast_gone = !has_active_toasts(1000 + TOAST_TICKS + 1);
    let history_kept = count() == before + 1; // still there after the toast expired
    toggle_centre();
    let opened = is_centre_open();
    toggle_centre();
    let closed = !is_centre_open();
    // BUG-057: what the desktop loop watches instead of repainting all the time.
    let g0 = generation();
    push("Test", "again", 5000);
    let counted = generation() == g0.wrapping_add(1) && active_toasts(5000) == 1 && active_toasts(5000 + TOAST_TICKS) == 0;
    toggle_centre();
    let (cx, cy, cw, ch) = centre_rect(1280, 720);
    let hit = centre_contains(cx + 5, cy + 5, 1280, 720) && !centre_contains(cx - 1, cy + 5, 1280, 720) && cx + cw <= 1280 && cy + ch <= 720;
    close_centre();
    let shut = !is_centre_open() && !centre_contains(cx + 5, cy + 5, 1280, 720);
    let ok = stored && toast_now && toast_gone && history_kept && opened && closed && counted && hit && shut;
    crate::serial_println!(
        "[notif] Notifications: posted-to-centre={stored}, toast-shows={toast_now}, toast-expires={toast_gone}, history-kept={history_kept}, centre-toggles={}, change-counters={counted}, list hit-test and close={} → {}",
        opened && closed,
        hit && shut,
        if ok { "OK (toasts + a notification centre) ✓" } else { "FAILED ✗" }
    );
}
