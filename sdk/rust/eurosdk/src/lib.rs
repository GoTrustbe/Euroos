//! # eurosdk: native EuroOS apps
//!
//! The Euro API is the interface the EuroOS kernel defines for applications:
//! a window surface you draw into, an event queue, the system text engine,
//! whole-file reads and writes inside your home, a log line and a clock.
//! Nothing here is Linux: no libc, no X11, no syscall of the Linux table. An
//! app is a freestanding static PIE (target `x86_64-unknown-none`) and runs
//! under the kernel's `euro` syscall profile, which allows exactly these calls.
//! Every call is gated by the EuroGuard capabilities the app's manifest
//! declares (`window`, `console`, `file`); a call without the right returns
//! `Err(Error::Denied)`.
//!
//! With the `wasm` feature the same functions become imports of the `euro`
//! module for eurowasm, and the app is frame-driven (see [`run_frames`]).
#![no_std]

/// The ABI generation this crate speaks. The kernel reports its own in [`info`].
pub const ABI_VERSION: u32 = 1;

/// Errors the Euro API returns (negative values on the wire).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    /// The manifest did not grant the capability this call needs.
    Denied,
    /// An argument was out of range (size, coordinates, length).
    Invalid,
    /// A pointer did not lie in the app's memory.
    Fault,
    /// The surface or file does not exist.
    NotFound,
    /// Too many or too large (second surface, file over the limit).
    Limit,
    /// Any other kernel error code.
    Other(i64),
}

fn err(r: u64) -> Result<u64, Error> {
    let v = r as i64;
    if v >= 0 {
        return Ok(r);
    }
    Err(match -v {
        1 => Error::Denied,
        22 => Error::Invalid,
        14 => Error::Fault,
        2 => Error::NotFound,
        7 | 27 | 28 => Error::Limit,
        e => Error::Other(e),
    })
}

/// Syscall numbers of the Euro API (`0x6100..`): the `euro` namespace.
pub mod nr {
    pub const INFO: u64 = 0x6100;
    pub const SURFACE_CREATE: u64 = 0x6101;
    pub const SURFACE_PRESENT: u64 = 0x6102;
    pub const EVENT_POLL: u64 = 0x6103;
    pub const EVENT_WAIT: u64 = 0x6104;
    pub const TEXT_DRAW: u64 = 0x6105;
    pub const TEXT_MEASURE: u64 = 0x6106;
    pub const LOG: u64 = 0x6107;
    pub const TIME_MS: u64 = 0x6108;
    pub const SLEEP_MS: u64 = 0x6109;
    pub const FILE_READ: u64 = 0x610A;
    pub const FILE_WRITE: u64 = 0x610B;
    pub const EXIT: u64 = 0x610C;
}

#[cfg(not(feature = "wasm"))]
mod sys {
    /// One raw Euro syscall: rax = number, rdi/rsi/rdx/r10/r8 = arguments.
    #[inline(always)]
    pub unsafe fn call(n: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> u64 {
        let r: u64;
        core::arch::asm!("syscall", inlateout("rax") n => r, in("rdi") a1, in("rsi") a2, in("rdx") a3,
            in("r10") a4, in("r8") a5, lateout("rcx") _, lateout("r11") _, options(nostack));
        r
    }
}

#[cfg(feature = "wasm")]
mod sys {
    #[link(wasm_import_module = "euro")]
    extern "C" {
        #[link_name = "call"]
        fn euro_call(n: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> u64;
    }
    #[inline(always)]
    pub unsafe fn call(n: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> u64 {
        euro_call(n, a1, a2, a3, a4, a5)
    }
}

/// What the kernel reports about itself and this process.
#[derive(Clone, Copy, Default, Debug)]
#[repr(C)]
pub struct Info {
    pub abi: u32,
    pub screen_w: u32,
    pub screen_h: u32,
    pub caps: u32,
}

/// Capability bits as the kernel reports them in [`Info::caps`].
pub mod caps {
    pub const CONSOLE: u32 = 1 << 0;
    pub const PROC_INFO: u32 = 1 << 1;
    pub const FILE: u32 = 1 << 2;
    pub const NET: u32 = 1 << 3;
    pub const WINDOW: u32 = 1 << 5;
}

/// Ask the kernel for its ABI version, the screen size and this app's rights.
pub fn info() -> Info {
    let mut i = Info::default();
    unsafe { sys::call(nr::INFO, &mut i as *mut Info as u64, core::mem::size_of::<Info>() as u64, 0, 0, 0) };
    i
}

/// One input or window event, 16 bytes on the wire.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct RawEvent {
    pub kind: u32,
    pub a: u32,
    pub b: u32,
    pub c: u32,
}

/// Decoded event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// A key went down or up: PC scancode (set 1), and the character it types
    /// under the active keyboard layout (0 if none).
    Key { scancode: u8, pressed: bool, ch: Option<char> },
    /// The pointer moved to (x, y) in surface coordinates.
    PointerMove { x: i32, y: i32 },
    /// A pointer button (1 = left, 2 = right, 3 = middle) went down or up at (x, y).
    Button { button: u8, pressed: bool, x: i32, y: i32 },
    /// The host resized the surface's window; present a new frame of this size.
    Resize { w: u32, h: u32 },
    /// The user closed the window: finish and exit.
    Close,
    /// An event this crate does not know (newer kernel).
    Unknown(RawEvent),
}

impl Event {
    pub fn from_raw(r: RawEvent) -> Event {
        match r.kind {
            1 => Event::Key { scancode: r.a as u8, pressed: r.b != 0, ch: char::from_u32(r.c).filter(|c| *c != '\0') },
            2 => Event::PointerMove { x: r.a as i32, y: r.b as i32 },
            3 => Event::Button { button: r.a as u8, pressed: r.b != 0, x: (r.c >> 16) as i32, y: (r.c & 0xffff) as i32 },
            4 => Event::Resize { w: r.a, h: r.b },
            5 => Event::Close,
            _ => Event::Unknown(r),
        }
    }
}

/// The next pending event, without waiting.
pub fn event_poll() -> Option<Event> {
    let mut r = RawEvent::default();
    let n = unsafe { sys::call(nr::EVENT_POLL, &mut r as *mut RawEvent as u64, 16, 0, 0, 0) };
    if n == 1 { Some(Event::from_raw(r)) } else { None }
}

/// Wait for an event to become pending, up to `timeout_ms`. `true` = one is
/// there. May return `false` before the timeout (the kernel sleeps in short
/// steps); loop on it.
pub fn event_wait(timeout_ms: u32) -> bool {
    unsafe { sys::call(nr::EVENT_WAIT, timeout_ms as u64, 0, 0, 0, 0) == 1 }
}

/// Write one line to the system log (and the app's console output). Needs `console`.
pub fn log(s: &str) -> Result<(), Error> {
    err(unsafe { sys::call(nr::LOG, s.as_ptr() as u64, s.len() as u64, 0, 0, 0) }).map(|_| ())
}

/// Milliseconds since boot.
pub fn time_ms() -> u64 {
    unsafe { sys::call(nr::TIME_MS, 0, 0, 0, 0, 0) }
}

/// Sleep for `ms` milliseconds.
pub fn sleep_ms(ms: u32) {
    unsafe { sys::call(nr::SLEEP_MS, ms as u64, 0, 0, 0, 0) };
}

/// End the process with `code`.
#[cfg(not(feature = "wasm"))]
pub fn exit(code: i32) -> ! {
    unsafe { sys::call(nr::EXIT, code as u64, 0, 0, 0, 0) };
    loop {}
}

/// End the app with `code`: the host records the code and the trap that
/// follows ends the current `init`/`frame` call cleanly.
#[cfg(feature = "wasm")]
pub fn exit(code: i32) -> ! {
    unsafe { sys::call(nr::EXIT, code as u64, 0, 0, 0, 0) };
    core::arch::wasm32::unreachable()
}

/// Read a whole file from your home into `buf`; returns the byte count. Needs
/// `file`. Paths are relative to the home directory or absolute inside it;
/// nothing outside it (not /tmp) is reachable.
pub fn file_read(path: &str, buf: &mut [u8]) -> Result<usize, Error> {
    err(unsafe {
        sys::call(nr::FILE_READ, path.as_ptr() as u64, path.len() as u64, buf.as_mut_ptr() as u64, buf.len() as u64, 0)
    })
    .map(|n| n as usize)
}

/// Write a whole file in your home. Needs `file`. At most 4 MiB per call and
/// 64 MiB per process lifetime (the files live in kernel memory until synced).
pub fn file_write(path: &str, data: &[u8]) -> Result<usize, Error> {
    err(unsafe {
        sys::call(nr::FILE_WRITE, path.as_ptr() as u64, path.len() as u64, data.as_ptr() as u64, data.len() as u64, 0)
    })
    .map(|n| n as usize)
}

/// Width and height in pixels that `s` takes at `px` pixels with the system font.
pub fn text_measure(s: &str, px: u32) -> (u32, u32) {
    let r = unsafe { sys::call(nr::TEXT_MEASURE, s.as_ptr() as u64, s.len() as u64, px as u64, 0, 0) };
    ((r >> 32) as u32, (r & 0xffff_ffff) as u32)
}

/// A window surface: the app owns the pixels (XRGB8888, row-major); the kernel
/// shows the last presented frame in a framed desktop window. One per process.
pub struct Surface<'a> {
    px: &'a mut [u32],
    w: u32,
    h: u32,
}

impl<'a> Surface<'a> {
    /// Create the window surface over your own pixel buffer (`buf.len() >= w*h`).
    /// Needs `window`.
    pub fn create(buf: &'a mut [u32], w: u32, h: u32) -> Result<Surface<'a>, Error> {
        if (w as usize) * (h as usize) > buf.len() || w == 0 || h == 0 {
            return Err(Error::Invalid);
        }
        err(unsafe { sys::call(nr::SURFACE_CREATE, w as u64, h as u64, 0, 0, 0) })?;
        Ok(Surface { px: buf, w, h })
    }
    pub fn width(&self) -> u32 { self.w }
    pub fn height(&self) -> u32 { self.h }
    /// The pixels, for your own drawing.
    pub fn pixels(&mut self) -> &mut [u32] { &mut self.px[..(self.w * self.h) as usize] }
    /// Hand the current pixels to the kernel: this is what the window shows.
    pub fn present(&self) -> Result<(), Error> {
        err(unsafe { sys::call(nr::SURFACE_PRESENT, self.px.as_ptr() as u64, self.w as u64, self.h as u64, 0, 0) }).map(|_| ())
    }
    pub fn fill(&mut self, argb: u32) {
        let n = (self.w * self.h) as usize;
        for p in &mut self.px[..n] { *p = argb; }
    }
    pub fn rect(&mut self, x: i32, y: i32, w: u32, h: u32, argb: u32) {
        let (sw, sh) = (self.w as i32, self.h as i32);
        for yy in y.max(0)..(y + h as i32).min(sh) {
            for xx in x.max(0)..(x + w as i32).min(sw) {
                self.px[(yy * sw + xx) as usize] = argb;
            }
        }
    }
    /// Draw `s` with the system font (anti-aliased, `px` pixels tall, 6..=96) at
    /// (x, y) = the top-left of the text box. Rendered by the kernel into your
    /// buffer. At most 512 bytes per call (draw longer text in pieces).
    pub fn text(&mut self, x: i32, y: i32, s: &str, argb: u32, px: u32) -> Result<(), Error> {
        let geom = ((x as u32 as u64) << 32) | (y as u32 as u64);
        let dims = ((self.w as u64) << 32) | self.h as u64;
        // a1 = buf, a2 = w<<32|h, a3 = x<<32|y, a4 = str ptr, a5 = len<<32 | px<<24 | ... colour
        // Colour and pixel size travel in the fifth argument: len (32) | px (8) | 0 | argb packed below.
        let a5 = ((s.len() as u64) << 40) | ((px as u64 & 0xff) << 32) | (argb as u64 & 0xff_ffff);
        err(unsafe { sys::call(nr::TEXT_DRAW, self.px.as_mut_ptr() as u64, dims, geom, s.as_ptr() as u64, a5) }).map(|_| ())
    }
}

/// Colours as XRGB8888.
pub mod color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> u32 { ((r as u32) << 16) | ((g as u32) << 8) | b as u32 }
    pub const WHITE: u32 = 0xff_ffff;
    pub const BLACK: u32 = 0x00_0000;
    pub const INK: u32 = rgb(20, 24, 33);
    pub const INK_SOFT: u32 = rgb(85, 92, 105);
    pub const ACCENT: u32 = rgb(45, 107, 224);
    pub const SURFACE: u32 = rgb(246, 247, 250);
}

/// Native entry glue: a `#[no_mangle] extern "C" fn _start()` that calls your
/// `main` and exits with its code. Use as `eurosdk::entry!(main);`.
#[cfg(not(feature = "wasm"))]
#[macro_export]
macro_rules! entry {
    ($main:path) => {
        #[no_mangle]
        pub extern "C" fn _start() -> ! {
            let f: fn() -> i32 = $main;
            $crate::exit(f())
        }
        #[panic_handler]
        fn panic(_info: &core::panic::PanicInfo) -> ! {
            let _ = $crate::log("panic");
            $crate::exit(101)
        }
    };
}

/// WASM entry glue: exports `init` and `frame` for the frame-driven host loop.
/// `init` runs once; `frame` runs on every desktop tick until it returns non-zero.
#[cfg(feature = "wasm")]
#[macro_export]
macro_rules! entry {
    ($init:path, $frame:path) => {
        #[export_name = "init"]
        pub extern "C" fn __euro_init() -> i32 { let f: fn() -> i32 = $init; f() }
        #[export_name = "frame"]
        pub extern "C" fn __euro_frame() -> i32 { let f: fn() -> i32 = $frame; f() }
        #[panic_handler]
        fn panic(_info: &core::panic::PanicInfo) -> ! {
            let _ = $crate::log("panic");
            $crate::exit(101)
        }
    };
}
