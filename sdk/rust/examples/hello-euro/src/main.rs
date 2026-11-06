//! hello-euro: a native EuroOS app. A window with a header, text rendered by the
//! system font, a counter of the keys and clicks it received, and a clean exit
//! on Escape or when the window is closed. No Linux library is involved: the
//! binary is a static PIE that only speaks the Euro API. The same source builds
//! for eurowasm (`--features wasm`), where the host drives `init`/`frame`.
#![no_std]
#![no_main]

use eurosdk::{color, Event, Surface};

const W: u32 = 520;
const H: u32 = 300;
static mut PX: [u32; (W * H) as usize] = [0; (W * H) as usize];
static mut SURF: Option<Surface<'static>> = None;
static mut ST: State = State { keys: 0, clicks: 0, last: None, pointer: (0, 0) };

struct State {
    keys: u32,
    clicks: u32,
    last: Option<char>,
    pointer: (i32, i32),
}

fn draw(s: &mut Surface, st: &State) {
    s.fill(color::WHITE);
    s.rect(0, 0, W, 64, color::ACCENT);
    let _ = s.text(20, 18, "Hello, EuroOS!", color::WHITE, 26);
    let _ = s.text(20, 88, "This window is a native EuroOS app:", color::INK, 17);
    let _ = s.text(20, 112, "no libc, no X11, no Linux syscall. Only the Euro API.", color::INK, 17);
    let mut line = [0u8; 64];
    let n = fmt(&mut line, b"keys: ", st.keys, b"   clicks: ", st.clicks);
    let _ = s.text(20, 160, core::str::from_utf8(&line[..n]).unwrap_or(""), color::INK_SOFT, 16);
    let mut pl = [0u8; 64];
    let m = fmt(&mut pl, b"pointer: ", st.pointer.0.max(0) as u32, b", ", st.pointer.1.max(0) as u32);
    let _ = s.text(20, 184, core::str::from_utf8(&pl[..m]).unwrap_or(""), color::INK_SOFT, 16);
    if let Some(c) = st.last {
        let mut b = [0u8; 4];
        let t = c.encode_utf8(&mut b);
        let _ = s.text(20, 208, "last key:", color::INK_SOFT, 16);
        let _ = s.text(100, 204, t, color::ACCENT, 22);
    }
    let _ = s.text(20, H as i32 - 36, "Esc or close the window to quit.", color::INK_SOFT, 14);
    let _ = s.present();
}

/// `prefix a mid b` into `out` without alloc or core::fmt.
fn fmt(out: &mut [u8; 64], p: &[u8], a: u32, m: &[u8], b: u32) -> usize {
    let mut n = 0;
    for &c in p { out[n] = c; n += 1; }
    n += num(&mut out[n..], a);
    for &c in m { out[n] = c; n += 1; }
    n += num(&mut out[n..], b);
    n
}
fn num(out: &mut [u8], mut v: u32) -> usize {
    let mut d = [0u8; 10];
    let mut i = 0;
    loop { d[i] = b'0' + (v % 10) as u8; v /= 10; i += 1; if v == 0 { break; } }
    for k in 0..i { out[k] = d[i - 1 - k]; }
    i
}

/// Create the surface and draw the first frame. 0 = ready, else an exit code.
fn init() -> i32 {
    let info = eurosdk::info();
    let _ = eurosdk::log("hello-euro: starting (Euro API)");
    if info.caps & eurosdk::caps::WINDOW == 0 {
        let _ = eurosdk::log("hello-euro: no window capability");
        return 2;
    }
    let buf = unsafe { &mut *core::ptr::addr_of_mut!(PX) };
    let s = match Surface::create(buf, W, H) {
        Ok(s) => s,
        Err(_) => { let _ = eurosdk::log("hello-euro: surface refused"); return 3; }
    };
    unsafe {
        SURF = Some(s);
        draw((*core::ptr::addr_of_mut!(SURF)).as_mut().unwrap(), &*core::ptr::addr_of!(ST));
    }
    let _ = eurosdk::log("hello-euro: first frame presented");
    0
}

/// Handle pending events and redraw. 0 = keep running, 1 = done.
fn frame() -> i32 {
    let st = unsafe { &mut *core::ptr::addr_of_mut!(ST) };
    let s = unsafe { (*core::ptr::addr_of_mut!(SURF)).as_mut() };
    let Some(s) = s else { return 1 };
    let mut dirty = false;
    while let Some(ev) = eurosdk::event_poll() {
        match ev {
            Event::Key { scancode, pressed: true, ch } => {
                if scancode == 0x01 { let _ = eurosdk::log("hello-euro: Escape, exiting"); return 1; }
                st.keys += 1; st.last = ch; dirty = true;
            }
            Event::Button { pressed: true, x, y, .. } => { st.clicks += 1; st.pointer = (x, y); dirty = true; }
            Event::PointerMove { x, y } => { st.pointer = (x, y); dirty = true; }
            Event::Close => { let _ = eurosdk::log("hello-euro: window closed, exiting"); return 1; }
            _ => {}
        }
    }
    if dirty { draw(s, st); }
    0
}

#[cfg(not(feature = "wasm"))]
fn main() -> i32 {
    let r = init();
    if r != 0 { return r; }
    loop {
        eurosdk::event_wait(250);
        if frame() != 0 { return 0; }
    }
}

#[cfg(not(feature = "wasm"))]
eurosdk::entry!(main);
#[cfg(feature = "wasm")]
eurosdk::entry!(init, frame);
