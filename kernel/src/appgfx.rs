//! Userspace app graphics + input bridge.
//!
//! A scheduled userspace program (e.g. the DOOM port) produces pixels and
//! consumes keystrokes, but it runs in its own address space on the preemptive
//! scheduler — it cannot touch the compositor directly. This module is the
//! hand-off point:
//!
//!   * `fb_present()` (syscall, from the app) copies the app's XRGB8888 frame
//!     into `FRAME` and marks it dirty.
//!   * the desktop loop calls `take_frame()` each iteration; if a new frame is
//!     ready it blits it (integer-scaled + centered) onto the real framebuffer.
//!   * while an app is active the keyboard handler routes keys to `push_key()`
//!     instead of the terminal; the app drains them with `getkey()` (syscall).
//!
//! No window chrome: an active app owns a centered rectangle over the desktop.
//! This keeps the compositor untouched and is exactly what a full-screen game
//! wants.

use alloc::vec::Vec;
use spin::Mutex;

/// The most recent frame handed over by the app: (pixels XRGB8888, w, h).
static FRAME: Mutex<Frame> = Mutex::new(Frame::new());
/// Pending key events: `(pressed as u16) << 8 | keycode`. Small ring; oldest
/// dropped on overflow so a stalled app never blocks input latching.
static KEYS: Mutex<Vec<u16>> = Mutex::new(Vec::new());
/// Whether an app currently owns the screen (set on spawn, cleared on exit).
static ACTIVE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// The pid of the app that owns the screen (the desktop loop polls its liveness
/// to release ownership when it exits — see `main.rs`).
static APP_PID: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
/// The framebuffer size, so an app (the browser) can render at native resolution
/// and map mouse coordinates 1:1.
static SCREEN_W: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
static SCREEN_H: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Record the framebuffer dimensions (called once at boot).
pub fn set_screen(w: usize, h: usize) {
    SCREEN_W.store(w as u32, core::sync::atomic::Ordering::Relaxed);
    SCREEN_H.store(h as u32, core::sync::atomic::Ordering::Relaxed);
}

/// The framebuffer dimensions (w, h).
pub fn screen() -> (usize, usize) {
    (
        SCREEN_W.load(core::sync::atomic::Ordering::Relaxed) as usize,
        SCREEN_H.load(core::sync::atomic::Ordering::Relaxed) as usize,
    )
}

/// Record the owning app's pid (set by the launcher alongside `set_active(true)`).
pub fn set_app_pid(pid: u64) {
    APP_PID.store(pid, core::sync::atomic::Ordering::Relaxed);
}

/// The pid of the app currently owning the screen (0 if none).
pub fn app_pid() -> u64 {
    APP_PID.load(core::sync::atomic::Ordering::Relaxed)
}

struct Frame {
    px: Vec<u32>,
    w: usize,
    h: usize,
    dirty: bool,
}

impl Frame {
    const fn new() -> Self {
        Frame { px: Vec::new(), w: 0, h: 0, dirty: false }
    }
}

/// True while an app owns the screen (the desktop loop should blit its frames
/// and the keyboard should feed it).
pub fn active() -> bool {
    ACTIVE.load(core::sync::atomic::Ordering::Relaxed)
}

/// Mark an app as owning / releasing the screen.
///
/// `set_active(false)` is LOCK-FREE (a single atomic store) so it is safe to call
/// from the syscall exit path, which runs under the held BG spinlock. The frame
/// + key buffers are cleared on the NEXT `set_active(true)` (launch context)
/// instead of here, so releasing never touches another subsystem's lock.
pub fn set_active(on: bool) {
    if on {
        // Launch: start clean (previous app's frame/keys discarded).
        let mut f = FRAME.lock();
        f.px.clear();
        f.w = 0;
        f.h = 0;
        f.dirty = false;
        KEYS.lock().clear();
    }
    ACTIVE.store(on, core::sync::atomic::Ordering::Relaxed);
}

/// App -> kernel: hand over a new frame (already validated + copied by the
/// syscall). `src` is `w*h` XRGB8888 pixels.
pub fn present(src: &[u32], w: usize, h: usize) {
    // Paint it to the screen RIGHT HERE, in the app's own syscall, so a
    // full-screen app (the DOOM port) shows every frame it produces instead of
    // waiting on the desktop loop (which the app itself can starve). The stored
    // FRAME below is only a fallback the desktop loop can still blit.
    if ACTIVE.load(core::sync::atomic::Ordering::Relaxed) {
        crate::screen_present_xrgb(&src[..w * h], w, h);
    }
    let mut f = FRAME.lock();
    if f.px.len() != w * h {
        f.px.clear();
        f.px.resize(w * h, 0);
    }
    f.px.copy_from_slice(&src[..w * h]);
    f.w = w;
    f.h = h;
    f.dirty = true;
}

/// Desktop loop -> take the current frame if it changed since last blit.
/// Returns `(pixels, w, h)`; the caller does the scaling/centering blit.
/// Runs the closure under the lock to avoid copying the whole frame out.
pub fn with_new_frame<R>(f: impl FnOnce(&[u32], usize, usize) -> R) -> Option<R> {
    let mut fr = FRAME.lock();
    if !fr.dirty || fr.w == 0 {
        return None;
    }
    fr.dirty = false;
    let (w, h) = (fr.w, fr.h);
    Some(f(&fr.px, w, h))
}

/// Keyboard handler -> queue a key for the app. `pressed` = down/up.
pub fn push_key(keycode: u8, pressed: bool) {
    let mut k = KEYS.lock();
    if k.len() > 64 {
        k.remove(0); // drop oldest; input never blocks
    }
    k.push(((pressed as u16) << 8) | keycode as u16);
}

/// App -> kernel: next key event or 0 if none (non-blocking).
pub fn getkey() -> u16 {
    let mut k = KEYS.lock();
    if k.is_empty() {
        0
    } else {
        k.remove(0)
    }
}
