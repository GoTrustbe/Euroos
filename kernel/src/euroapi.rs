//! # Euro API v1: the kernel interface for native EuroOS apps
//!
//! Sprint WP6 (`docs/SPRINT-PLAN-NATIVE-APPS.md`). A native app is a
//! freestanding static PIE that speaks only this interface: a window surface
//! it draws into, an event queue, the system text engine, whole-file reads
//! and writes inside the user's home, a log line and a clock. No Linux
//! library, no Linux syscall: the app runs under `Profile::Euro`, an
//! allow-list of exactly these numbers plus exit, memory and sleep.
//!
//! Syscall numbers `0x6100..0x610C` live in the `euro` namespace; the arms
//! in `ring3::bg_dispatch` validate every user pointer and size and call in
//! here. This module owns the surfaces (kernel-side copies of the last
//! presented frame, shown by the compositor as a framed window), the
//! per-surface event queue, keyboard focus and the text renderer.
//!
//! Security properties, each tested: sizes bounded (surface <= 1920x1200,
//! strings <= 4 KiB, files <= 16 MiB, queue 256); one surface per process;
//! events only to the focused surface; file paths confined to the session
//! user's home and `/tmp`; every call capability-gated (`CAP_WINDOW`,
//! `CAP_CONSOLE`, `CAP_FILE`).

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use spin::Mutex;

pub const ABI_VERSION: u32 = 1;
pub const MAX_W: usize = 1920;
pub const MAX_H: usize = 1200;
pub const MAX_STR: usize = 4096;
pub const MAX_FILE: usize = 16 << 20;
const MAX_EVENTS: usize = 256;

pub const EV_KEY: u32 = 1;
pub const EV_MOVE: u32 = 2;
pub const EV_BUTTON: u32 = 3;
pub const EV_RESIZE: u32 = 4;
pub const EV_CLOSE: u32 = 5;

/// One event, 16 bytes on the wire (`kind, a, b, c`).
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct Event {
    pub kind: u32,
    pub a: u32,
    pub b: u32,
    pub c: u32,
}

impl Event {
    pub fn to_bytes(self) -> [u8; 16] {
        let mut b = [0u8; 16];
        b[0..4].copy_from_slice(&self.kind.to_le_bytes());
        b[4..8].copy_from_slice(&self.a.to_le_bytes());
        b[8..12].copy_from_slice(&self.b.to_le_bytes());
        b[12..16].copy_from_slice(&self.c.to_le_bytes());
        b
    }
}

struct Surface {
    pid: u64,
    w: usize,
    h: usize,
    px: Vec<u32>,
    dirty: bool,
    frames: u64,
    events: VecDeque<Event>,
    dropped: u64,
    shift: bool,
}

static SURFACES: Mutex<Vec<Surface>> = Mutex::new(Vec::new());
/// The process whose surface has keyboard focus and is shown in the hosted window (0 = none).
static FOCUS: AtomicU64 = AtomicU64::new(0);
/// Set when a surface was created or presented; the desktop takes it to repaint.
static DIRTY: AtomicBool = AtomicBool::new(false);
/// Total events delivered (self-test and `euro` shell diagnostics).
pub static EVENTS_DELIVERED: AtomicU64 = AtomicU64::new(0);

const EPERM: i64 = -1;
const ENOENT: i64 = -2;
const EEXIST: i64 = -17;
const EINVAL: i64 = -22;
const EFBIG: i64 = -27;

/// surface_create: one surface per process, bounded size. Takes focus.
pub fn create(pid: u64, w: usize, h: usize) -> i64 {
    if w == 0 || h == 0 || w > MAX_W || h > MAX_H {
        return EINVAL;
    }
    let mut s = SURFACES.lock();
    if s.iter().any(|x| x.pid == pid) {
        return EEXIST;
    }
    s.push(Surface { pid, w, h, px: alloc::vec![0xff20_2020u32; w * h], dirty: false, frames: 0, events: VecDeque::new(), dropped: 0, shift: false });
    FOCUS.store(pid, Ordering::Relaxed);
    DIRTY.store(true, Ordering::Relaxed);
    crate::serial_println!("[euroapi] pid {pid}: surface {w}x{h} created");
    1
}

/// surface_present: copy the app's frame (XRGB8888, `w*h` pixels, already
/// validated to lie in the app's memory) into the kernel-side surface. A
/// different size than at creation resizes the surface (within bounds).
pub fn present(pid: u64, src: &[u32], w: usize, h: usize) -> i64 {
    if w == 0 || h == 0 || w > MAX_W || h > MAX_H || src.len() < w * h {
        return EINVAL;
    }
    let mut s = SURFACES.lock();
    let Some(sf) = s.iter_mut().find(|x| x.pid == pid) else { return ENOENT };
    if sf.w != w || sf.h != h {
        sf.w = w;
        sf.h = h;
        sf.px = alloc::vec![0u32; w * h];
    }
    sf.px.copy_from_slice(&src[..w * h]);
    sf.frames += 1;
    sf.dirty = true;
    DIRTY.store(true, Ordering::Relaxed);
    0
}

/// event_poll: the oldest pending event of this process's surface.
pub fn poll(pid: u64) -> Option<Event> {
    let mut s = SURFACES.lock();
    let sf = s.iter_mut().find(|x| x.pid == pid)?;
    sf.events.pop_front()
}

/// Is an event pending (for event_wait, which sleeps outside the lock)?
pub fn pending(pid: u64) -> bool {
    SURFACES.lock().iter().find(|x| x.pid == pid).map(|x| !x.events.is_empty()).unwrap_or(false)
}

pub fn has_surface(pid: u64) -> bool {
    SURFACES.lock().iter().any(|x| x.pid == pid)
}

pub fn frames(pid: u64) -> u64 {
    SURFACES.lock().iter().find(|x| x.pid == pid).map(|x| x.frames).unwrap_or(0)
}

/// Drop the surface of an exited process; focus moves to the newest remaining one.
pub fn remove(pid: u64) {
    let mut s = SURFACES.lock();
    s.retain(|x| x.pid != pid);
    if FOCUS.load(Ordering::Relaxed) == pid {
        FOCUS.store(s.last().map(|x| x.pid).unwrap_or(0), Ordering::Relaxed);
        DIRTY.store(true, Ordering::Relaxed);
    }
}

pub fn focus() -> Option<u64> {
    let f = FOCUS.load(Ordering::Relaxed);
    if f == 0 { None } else { Some(f) }
}

pub fn set_focus(pid: u64) {
    if has_surface(pid) {
        FOCUS.store(pid, Ordering::Relaxed);
        DIRTY.store(true, Ordering::Relaxed);
    }
}

/// The desktop takes the repaint flag once per frame.
pub fn take_dirty() -> bool {
    DIRTY.swap(false, Ordering::Relaxed)
}

/// The focused surface's size, for the compositor's hit testing.
pub fn front_size() -> Option<(usize, usize)> {
    let f = FOCUS.load(Ordering::Relaxed);
    SURFACES.lock().iter().find(|x| x.pid == f).map(|x| (x.w, x.h))
}

/// Run `f` over the focused surface's pixels (what the hosted window shows).
pub fn with_front<R>(f: impl FnOnce(usize, usize, &[u32]) -> R) -> Option<R> {
    let focus = FOCUS.load(Ordering::Relaxed);
    let mut s = SURFACES.lock();
    let sf = s.iter_mut().find(|x| x.pid == focus)?;
    sf.dirty = false;
    Some(f(sf.w, sf.h, &sf.px))
}

fn push(pid: u64, ev: Event) {
    let mut s = SURFACES.lock();
    if let Some(sf) = s.iter_mut().find(|x| x.pid == pid) {
        if sf.events.len() >= MAX_EVENTS {
            sf.events.pop_front();
            sf.dropped += 1;
        }
        sf.events.push_back(ev);
        EVENTS_DELIVERED.fetch_add(1, Ordering::Relaxed);
    }
}

/// A raw PC scancode (set 1, bit 7 = release) for the FOCUSED surface only. The
/// character under the active keyboard layout travels with it.
pub fn push_key(sc: u8, pressed: bool) {
    let Some(pid) = focus() else { return };
    let code = sc & 0x7f;
    let shift = {
        let mut s = SURFACES.lock();
        let Some(sf) = s.iter_mut().find(|x| x.pid == pid) else { return };
        if code == 0x2a || code == 0x36 {
            sf.shift = pressed;
        }
        sf.shift
    };
    let ch = if pressed { eurokeymap::translate(crate::ps2::layout(), code, shift) } else { None };
    push(pid, Event { kind: EV_KEY, a: code as u32, b: pressed as u32, c: ch.map(|c| c as u32).unwrap_or(0) });
}

pub fn push_move(x: i32, y: i32) {
    if let Some(pid) = focus() {
        push(pid, Event { kind: EV_MOVE, a: x as u32, b: y as u32, c: 0 });
    }
}

pub fn push_button(button: u8, pressed: bool, x: i32, y: i32) {
    if let Some(pid) = focus() {
        let c = ((x.clamp(0, 0xffff) as u32) << 16) | (y.clamp(0, 0xffff) as u32);
        push(pid, Event { kind: EV_BUTTON, a: button as u32, b: pressed as u32, c });
    }
}

pub fn push_resize(w: u32, h: u32) {
    if let Some(pid) = focus() {
        push(pid, Event { kind: EV_RESIZE, a: w, b: h, c: 0 });
    }
}

pub fn push_close() {
    if let Some(pid) = focus() {
        push(pid, Event { kind: EV_CLOSE, a: 0, b: 0, c: 0 });
    }
}

/// Deliver a key to a specific process (self-test), bypassing focus.
pub fn inject_key_to(pid: u64, sc: u8, pressed: bool) {
    let ch = if pressed { eurokeymap::translate(crate::ps2::layout(), sc & 0x7f, false) } else { None };
    push(pid, Event { kind: EV_KEY, a: (sc & 0x7f) as u32, b: pressed as u32, c: ch.map(|c| c as u32).unwrap_or(0) });
}

/// text_draw: render `s` with the system UI font into the app's own pixel
/// buffer (`px`, `w*h`, validated by the caller) at (x, y) = top-left of the
/// text line, `size` pixels tall, colour `argb`. Clipped to the buffer.
pub fn text_draw(px: &mut [u32], w: usize, h: usize, x: i32, y: i32, s: &str, argb: u32, size: u32) -> i64 {
    if w == 0 || h == 0 || w > MAX_W || h > MAX_H || px.len() < w * h || s.len() > MAX_STR {
        return EINVAL;
    }
    if x < 0 || y < 0 || x as usize >= w || y as usize >= h {
        return 0; // entirely outside: nothing to draw, not an error
    }
    let size = size.clamp(6, 200) as f32;
    let fb = crate::graphics::FrameBuffer::over_pixels(px.as_mut_ptr(), w, h);
    crate::text::draw_px_styled(&fb, x as usize, y as usize, s, crate::graphics::Color::from_u32(argb), size, 0, 0);
    0
}

/// text_measure: (width, height) of `s` at `size` pixels, packed `w << 32 | h`.
pub fn text_measure(s: &str, size: u32) -> u64 {
    let size = size.clamp(6, 200) as f32;
    let w = crate::text::width_px_styled(s, size, 0, 0) as u64;
    let h = (size * 1.25) as u64;
    (w << 32) | h
}

/// The file scope of a native app: inside the session user's home or /tmp.
/// Returns the absolute path, or None when the path leaves the scope.
pub fn file_path_ok(path: &str) -> Option<String> {
    if path.is_empty() || path.len() > 512 || path.bytes().any(|b| b == 0) {
        return None;
    }
    if path.split('/').any(|c| c == "..") {
        return None;
    }
    let user = crate::session::active().map(|s| s.name).unwrap_or_else(|| String::from("euro"));
    let home = alloc::format!("/home/{user}");
    let abs = if let Some(rest) = path.strip_prefix('/') {
        alloc::format!("/{rest}")
    } else {
        alloc::format!("{home}/{path}")
    };
    if abs.starts_with(&alloc::format!("{home}/")) || abs.starts_with("/tmp/") {
        Some(abs)
    } else {
        None
    }
}

/// Lines the `euro` shell diagnostic shows.
pub fn status_lines() -> Vec<String> {
    let s = SURFACES.lock();
    let mut out = Vec::new();
    out.push(alloc::format!("Euro API v{ABI_VERSION}: {} surface(s), focus pid {}, events delivered {}",
        s.len(), FOCUS.load(Ordering::Relaxed), EVENTS_DELIVERED.load(Ordering::Relaxed)));
    for sf in s.iter() {
        out.push(alloc::format!("  pid {}: {}x{}, {} frame(s), {} pending event(s), {} dropped", sf.pid, sf.w, sf.h, sf.frames, sf.events.len(), sf.dropped));
    }
    out
}

/// The colour of one pixel of a process's surface (self-test).
pub fn pixel(pid: u64, x: usize, y: usize) -> Option<u32> {
    let s = SURFACES.lock();
    let sf = s.iter().find(|p| p.pid == pid)?;
    if x < sf.w && y < sf.h { Some(sf.px[y * sf.w + x] & 0xff_ffff) } else { None }
}

#[cfg(test)]
mod tests {
    // The pure parts (event packing, path scope) are covered by the kernel's
    // boot self-test `[euroapp]`; the kernel crate has no host test harness.
}
