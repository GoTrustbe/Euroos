//! Minimal X11 server (core protocol) — enough for a real Xlib/XCB client to
//! connect (XOpenDisplay), create + map a window, and draw into it, with the
//! output composited by the EuroOS desktop. This is the display-server rung of
//! the "run GUI Linux apps (→ Chromium)" path.
//!
//! Transport: a client connect()s to the AF_UNIX X socket (`/tmp/.X11-unix/X0`,
//! abstract or filesystem); that fd routes here. The server is REACTIVE — it
//! parses requests out of the bytes the client write()s and queues replies/events
//! for the client to read(). No background task: the handshake + requests are
//! processed inline on the write() syscall.

use alloc::vec::Vec;
use spin::Mutex;

/// X connections are a class of objects of their own, beside the AF_INET and
/// AF_UNIX classes (net.rs) and above the file tables.
pub const XCONN_FD_BASE: u64 = 0x1_2000;
const MAX_XCONN: usize = 8;

/// Default screen geometry reported in the setup (refined to the real framebuffer
/// once window mapping is wired to the compositor).
static SCREEN_W: core::sync::atomic::AtomicU16 = core::sync::atomic::AtomicU16::new(1280);
static SCREEN_H: core::sync::atomic::AtomicU16 = core::sync::atomic::AtomicU16::new(800);
fn screen_w() -> u16 { SCREEN_W.load(core::sync::atomic::Ordering::Relaxed) }
fn screen_h() -> u16 { SCREEN_H.load(core::sync::atomic::Ordering::Relaxed) }

/// The size of the X screen as clients see it in the connection setup, and the
/// largest size a window can be given. The desktop sets it to the framebuffer
/// before it starts a hosted client; 1280x800 until then.
pub fn set_screen(w: usize, h: usize) {
    SCREEN_W.store(w.clamp(640, 8192) as u16, core::sync::atomic::Ordering::Relaxed);
    SCREEN_H.store(h.clamp(480, 8192) as u16, core::sync::atomic::Ordering::Relaxed);
}

// Fixed resource ids we hand out in the setup for the root window / visual / colormap.
const ROOT_WINDOW: u32 = 0x0000_0001;
const ROOT_VISUAL: u32 = 0x0000_0021;
const DEFAULT_CMAP: u32 = 0x0000_0020;
const RID_BASE: u32 = 0x0040_0000;
const RID_MASK: u32 = 0x001f_ffff;

#[derive(PartialEq)]
enum State {
    PreSetup,
    Connected,
}

/// A client window: geometry + an XRGB8888 pixel buffer we draw into and present.
struct XWindow {
    id: u32,
    x: i16,
    y: i16,
    w: u16,
    h: u16,
    mapped: bool,
    event_mask: u32, // XSelectInput: which events this window wants
    buf: Vec<u32>,
    /// CWBackPixel: what ClearArea and a fresh window show (opaque ARGB).
    bg: u32,
}

/// A graphics context — for the first rendering rung we only track the fill colour.
struct XGc {
    id: u32,
    fg: u32,
    /// GCBackground: the fill behind ImageText8 glyphs.
    bg: u32,
}

/// An off-screen drawable. Toolkits (GTK) render their widget tree into a pixmap,
/// then CopyArea it onto the window — so a pixmap needs a real backing buffer.
struct XPixmap {
    id: u32,
    w: u16,
    h: u16,
    buf: Vec<u32>,
}

struct XConn {
    /// This connection's resource-id-base (distinct per connection).
    rid_base: u32,
    inbuf: Vec<u8>,   // accumulated request bytes not yet consumed
    outbuf: Vec<u8>,  // reply/event bytes waiting to be read()
    state: State,
    seq: u16,         // last request's sequence number (server-side counter)
    swap: bool,       // client is big-endian (byte-swap multi-byte fields)
    windows: Vec<XWindow>,
    gcs: Vec<XGc>,
    pixmaps: Vec<XPixmap>,
}

static XCONNS: Mutex<[Option<XConn>; MAX_XCONN]> = Mutex::new([const { None }; MAX_XCONN]);

/// Trace X requests to serial (bring-up debugging).
pub static TRACE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

pub fn is_xconn_fd(fd: u64) -> bool {
    fd >= XCONN_FD_BASE && (fd - XCONN_FD_BASE) < MAX_XCONN as u64
}

/// A client connected to the X socket → allocate a connection. Returns its fd.
pub fn open() -> Option<u64> {
    let mut t = XCONNS.lock();
    for (i, s) in t.iter_mut().enumerate() {
        if s.is_none() {
            // A DISTINCT resource-id-base per connection: this is exactly what the
            // field in the setup reply exists for. Handing every client the same
            // base let chrome's second connection create "window 0x400003" while the
            // first connection's browser window carried that id too — and the screen
            // then presented the newcomer's 1x1 stub instead of the painted 800x600
            // browser frame.
            *s = Some(XConn { inbuf: Vec::new(), outbuf: Vec::new(), state: State::PreSetup, seq: 0, swap: false, windows: Vec::new(), gcs: Vec::new(), pixmaps: Vec::new(), rid_base: RID_BASE + (i as u32) * 0x0020_0000 });
            trace(format_args!("client connected -> xconn fd {}", XCONN_FD_BASE + i as u64));
            return Some(XCONN_FD_BASE + i as u64);
        }
    }
    None
}

pub fn close(fd: u64) {
    if is_xconn_fd(fd) {
        let idx = (fd - XCONN_FD_BASE) as usize;
        XCONNS.lock()[idx] = None;
        SEL_OWNERS.lock().retain(|&(_, _, c)| c != idx);
        XFIXES_SELECT.lock().retain(|e| e.0 != idx);
    }
}

/// The atom table: one id per name for every client, as the protocol says. (It
/// used to be a fresh number per InternAtom call, so two clients, or one client
/// asking twice, never agreed on what CLIPBOARD is.) Ids 1..=68 are predefined.
const PREDEFINED_ATOMS: [&str; 68] = [
    "PRIMARY", "SECONDARY", "ARC", "ATOM", "BITMAP", "CARDINAL", "COLORMAP", "CURSOR",
    "CUT_BUFFER0", "CUT_BUFFER1", "CUT_BUFFER2", "CUT_BUFFER3", "CUT_BUFFER4", "CUT_BUFFER5",
    "CUT_BUFFER6", "CUT_BUFFER7", "DRAWABLE", "FONT", "INTEGER", "PIXMAP", "POINT", "RECTANGLE",
    "RESOURCE_MANAGER", "RGB_COLOR_MAP", "RGB_BEST_MAP", "RGB_BLUE_MAP", "RGB_DEFAULT_MAP",
    "RGB_GRAY_MAP", "RGB_GREEN_MAP", "RGB_RED_MAP", "STRING", "VISUALID", "WINDOW", "WM_COMMAND",
    "WM_HINTS", "WM_CLIENT_MACHINE", "WM_ICON_NAME", "WM_ICON_SIZE", "WM_NAME", "WM_NORMAL_HINTS",
    "WM_SIZE_HINTS", "WM_ZOOM_HINTS", "MIN_SPACE", "NORM_SPACE", "MAX_SPACE", "END_SPACE",
    "SUPERSCRIPT_X", "SUPERSCRIPT_Y", "SUBSCRIPT_X", "SUBSCRIPT_Y", "UNDERLINE_POSITION",
    "UNDERLINE_THICKNESS", "STRIKEOUT_ASCENT", "STRIKEOUT_DESCENT", "ITALIC_ANGLE", "X_HEIGHT",
    "QUAD_WIDTH", "WEIGHT", "POINT_SIZE", "RESOLUTION", "COPYRIGHT", "NOTICE", "FONT_NAME",
    "FAMILY_NAME", "FULL_NAME", "CAP_HEIGHT", "WM_CLASS", "WM_TRANSIENT_FOR",
];
static ATOMS: Mutex<Vec<alloc::string::String>> = Mutex::new(Vec::new());

/// The atom of `name`; 0 (None) when it does not exist and `only_if_exists`.
fn intern_atom(name: &str, only_if_exists: bool) -> u32 {
    if let Some(i) = PREDEFINED_ATOMS.iter().position(|a| *a == name) {
        return i as u32 + 1;
    }
    let mut t = ATOMS.lock();
    if let Some(i) = t.iter().position(|a| a == name) {
        return 69 + i as u32;
    }
    if only_if_exists || t.len() >= 4096 {
        return 0;
    }
    t.push(alloc::string::String::from(name));
    68 + t.len() as u32
}

fn atom_name(atom: u32) -> Option<alloc::string::String> {
    match atom {
        1..=68 => Some(alloc::string::String::from(PREDEFINED_ATOMS[atom as usize - 1])),
        _ => ATOMS.lock().get((atom as usize).checked_sub(69)?).cloned(),
    }
}

/// The hosted desktop client (Chromium) gets the parts of a display server that
/// the boot-phase test clients were never offered: XFIXES selection events and
/// PropertyNotify. Set by the desktop before it starts the client.
static DESKTOP_CLIENT: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
pub fn set_desktop_client(on: bool) {
    DESKTOP_CLIENT.store(on, core::sync::atomic::Ordering::Relaxed);
    if !on {
        XFIXES_SELECT.lock().clear();
    }
}
/// XFIXES: major opcode, first event, first error (QueryExtension hands them out).
const XFIXES_OPCODE: u8 = 138;
const XFIXES_EVENT: u8 = 87;
const XFIXES_ERROR: u8 = 140;
/// XFixesSelectSelectionInput: (connection, window, selection atom, event mask).
static XFIXES_SELECT: Mutex<Vec<(usize, u32, u32, u32)>> = Mutex::new(Vec::new());

/// Who owns each selection: (selection atom, owner window, owner connection).
/// Chrome copies by taking CLIPBOARD and then ASKS who owns it; only when the
/// answer is its own window does it keep what was copied.
static SEL_OWNERS: Mutex<Vec<(u32, u32, usize)>> = Mutex::new(Vec::new());

/// The keyboard layout changed (`keymap`, the installer, the settings): every
/// client gets a MappingNotify, on which it fetches the keyboard mapping again.
pub fn keymap_changed() {
    x86_64::instructions::interrupts::without_interrupts(|| {
        for conn in lock_home().iter_mut().flatten() {
            if conn.state != State::Connected {
                continue;
            }
            let mut e = [0u8; 32];
            e[0] = 34; // MappingNotify
            e[2..4].copy_from_slice(&conn.seq.to_le_bytes());
            e[4] = 1; // request = Keyboard
            e[5] = 8; // first keycode
            e[6] = 248; // count
            conn.outbuf.extend_from_slice(&e);
        }
    });
}

/// The desktop gave the hosted window's frame another size (resize grip,
/// maximise, a smaller screen): the X window gets that size, a ConfigureNotify
/// and an Expose, and the client lays itself out again and repaints.
pub fn hosted_resize(w: usize, h: usize) {
    let (w, h) = (w.clamp(64, screen_w() as usize) as u16, h.clamp(64, screen_h() as usize) as u16);
    x86_64::instructions::interrupts::without_interrupts(|| {
        let shown = RETAINED_ID.load(core::sync::atomic::Ordering::Relaxed);
        if shown == 0 {
            return;
        }
        let mut t = lock_home();
        let Some(ci) = conn_of_window(&t[..], shown) else { return };
        let Some(conn) = t[ci].as_mut() else { return };
        let mut at = None;
        if let Some(win) = conn.windows.iter_mut().find(|x| x.id == shown) {
            if win.w != w || win.h != h {
                win.w = w;
                win.h = h;
                win.buf = alloc::vec![win.bg; w as usize * h as usize];
                at = Some((win.x, win.y));
            }
        }
        if let Some((x, y)) = at {
            send_configure_notify(conn, shown, x, y, w, h);
            send_expose(conn, shown, w, h);
            crate::serial_println!("[xserver] hosted window {shown:#x} resized to {w}x{h} by the desktop");
        }
    });
}

/// The client wrote `data` — feed the protocol state machine, producing replies.
pub fn write(fd: u64, data: &[u8]) -> u64 {
    // Take the connection OUT of the table while processing its requests, so the
    // drawing ops can reach SIBLING connections: X resource ids are server-global
    // (chrome's viz component paints, over its own connection, into the window the
    // browser connection created), and holding the table lock across processing
    // made that lookup impossible.
    let idx = (fd - XCONN_FD_BASE) as usize;
    if idx >= MAX_XCONN {
        return (-9i64) as u64;
    }
    let mut c = {
        let mut t = XCONNS.lock();
        match t[idx].take() {
            Some(c) => {
                // Marked under the table lock: see lock_home().
                OUT_MASK.fetch_or(1 << idx, core::sync::atomic::Ordering::Relaxed);
                c
            }
            None => return (-9i64) as u64, // -EBADF
        }
    };
    c.inbuf.extend_from_slice(data);
    process(&mut c);
    {
        let mut t = XCONNS.lock();
        t[idx] = Some(c);
        OUT_MASK.fetch_and(!(1 << idx), core::sync::atomic::Ordering::Relaxed);
    }
    data.len() as u64
}

/// Connections that are OUT of the table right now (bit = index): `write` takes
/// a connection out while it processes that client's requests.
static OUT_MASK: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// The connection table with EVERY connection in it, for whoever has an event to
/// deliver. An input event that found its connection out of the table (the
/// client busy in a PutImage on another core) had no owner to go to and was
/// dropped: a key of a typed word, the release of a click, one press of a double
/// click (BUG-094). The wait is as long as one batch of requests takes, on
/// another core. On one core it never waits: a syscall is not preempted. Bounded
/// anyway, so a mistake here costs an event and not the machine.
fn lock_home() -> spin::MutexGuard<'static, [Option<XConn>; MAX_XCONN]> {
    let mut spins = 0u32;
    loop {
        let t = XCONNS.lock();
        if OUT_MASK.load(core::sync::atomic::Ordering::Relaxed) == 0 || spins > 20_000_000 {
            if spins > 20_000_000 {
                static SAID: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
                if !SAID.swap(true, core::sync::atomic::Ordering::Relaxed) {
                    crate::serial_println!("[xserver] a connection stayed out of the table for 20M spins (mask {:#x}): delivering without it",
                        OUT_MASK.load(core::sync::atomic::Ordering::Relaxed));
                }
            }
            return t;
        }
        drop(t);
        if spins == 0 {
            HOME_WAITS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        }
        spins += 1;
        core::hint::spin_loop();
    }
}
/// How often an event had to wait for a connection that was out of the table
/// (each of these was a dropped event before BUG-094). `cdiag` prints it.
static HOME_WAITS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// The client is reading — drain up to `max` bytes of queued replies/events.
/// Connections that have DRAINED events (called read and got bytes) — the ones with
/// a live event loop pumping this socket. A window event whose selector never reads
/// (chrome's synchronous browser connection selects but does not poll) is ALSO
/// delivered to these, because that is the thread chrome routes X input to.
static READER_CONNS: Mutex<Vec<usize>> = Mutex::new(Vec::new());

fn mark_reader(idx: usize) {
    let mut r = READER_CONNS.lock();
    if !r.contains(&idx) {
        r.push(idx);
    }
}

pub fn read(fd: u64, max: usize) -> Vec<u8> {
    let mut t = XCONNS.lock();
    let c = match t.get_mut((fd - XCONN_FD_BASE) as usize).and_then(|s| s.as_mut()) {
        Some(c) => c,
        None => return Vec::new(),
    };
    let n = max.min(c.outbuf.len());
    if n > 0 {
        trace(format_args!("client read {n} B ({} left queued) conn={}",
            c.outbuf.len() - n, (fd - XCONN_FD_BASE) as usize));
        mark_reader((fd - XCONN_FD_BASE) as usize);
    }
    let out: Vec<u8> = c.outbuf.drain(0..n).collect();
    if n >= 32 {
        keylat::read();
    }
    // Did this read carry a ButtonPress/Release out to the client? Events are
    // 32-byte records and replies are 32-byte-multiples, so the opcode of every
    // 32-byte chunk is at a known place; 4/5 in the low 7 bits is press/release.
    // Unconditional and rare: it fires once per clicked button, not per request.
    for chunk in out.chunks(32) {
        let op = chunk[0] & 0x7f;
        if op == 4 || op == 5 {
            crate::serial_println!("[xin] <- conn={} client DRAINED kind={op} ({n} B read, {} B left)",
                (fd - XCONN_FD_BASE) as usize, c.outbuf.len());
        }
    }
    out
}

/// How many bytes are queued for this connection's client to collect.
pub fn queued_len(fd: u64) -> usize {
    XCONNS
        .lock()
        .get((fd - XCONN_FD_BASE) as usize)
        .and_then(|s| s.as_ref())
        .map(|c| c.outbuf.len())
        .unwrap_or(0)
}

/// Any queued output to read? (For a future poll/select.)
pub fn readable(fd: u64) -> bool {
    XCONNS
        .lock()
        .get((fd - XCONN_FD_BASE) as usize)
        .and_then(|s| s.as_ref())
        .map(|c| !c.outbuf.is_empty())
        .unwrap_or(false)
}

/// The window an input event goes to: the LARGEST mapped window that selected the
/// wanted mask (a toplevel, never a 1x1 input-only child or an off-screen stub).
/// Returns (id, x, y) so the caller can turn screen coordinates into window-local
/// ones — chrome hit-tests its tab strip and toolbar on event-x/event-y, so a
/// window at (40,40) fed screen coordinates misses every target by that offset.
fn input_target(c: &XConn, want: u32) -> Option<(u32, i16, i16)> {
    c.windows
        .iter()
        .filter(|w| w.mapped && w.w > 1 && w.h > 1 && (want == 0 || w.event_mask & want != 0))
        .max_by_key(|w| w.w as u32 * w.h as u32)
        .map(|w| (w.id, w.x, w.y))
}

/// The X modifier/button state (the `state` field of every input event): shift/ctrl/
/// alt as the keyboard sees them, plus button 1 while it is held. Chrome reads this
/// for shift-click, ctrl-click and drag; a hardcoded 0 makes every event a plain one.
static MOD_STATE: core::sync::atomic::AtomicU16 = core::sync::atomic::AtomicU16::new(0);
fn mod_state() -> u16 {
    // Button1 = left (0x100), Button2 = middle (0x200), Button3 = right (0x400).
    let b = crate::mouse::buttons();
    MOD_STATE.load(core::sync::atomic::Ordering::Relaxed)
        | if b & 0x1 != 0 { 0x100 } else { 0 }
        | if b & 0x4 != 0 { 0x200 } else { 0 }
        | if b & 0x2 != 0 { 0x400 } else { 0 }
}
/// An `E0` prefix was read from the scancode ring and its code is still to come.
static KBD_EXT: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// The pointer's position in the hosted client's root coordinates (x << 16 | y),
/// as the last event sent to it said. QueryPointer answers with it.
static LAST_ROOT: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// The window a held mouse button belongs to (0 = no button held): X's implicit
/// pointer grab, from the press to the release.
static IMPLICIT_GRAB: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// The keyboard half of the server's input state, for `cdiag`: the modifier
/// word that goes into every event, the focus window, a pending `E0`.
pub fn dump_input_state() {
    crate::serial_println!("[xin] state: modifiers={:#x} (1 Shift, 2 Lock, 4 Control, 8 Alt, 0x80 AltGr) buttons={:#x} focus={:#x} shown={:#x} E0-pending={}",
        MOD_STATE.load(core::sync::atomic::Ordering::Relaxed), crate::mouse::buttons(),
        FOCUS_WINDOW.load(core::sync::atomic::Ordering::Relaxed),
        RETAINED_ID.load(core::sync::atomic::Ordering::Relaxed),
        KBD_EXT.load(core::sync::atomic::Ordering::Relaxed));
    let sel: Vec<_> = SEL_OWNERS.lock().clone();
    crate::serial_println!("[xin] selections owned (atom, window, connection): {sel:?}");
    crate::serial_println!("[xin] events that waited for a connection busy on another core: {}",
        HOME_WAITS.load(core::sync::atomic::Ordering::Relaxed));
    let t = lock_home();
    for (ci, c) in t.iter().enumerate() {
        if let Some(c) = c {
            let wins: Vec<_> = c.windows.iter().filter(|w| w.w > 1 && w.h > 1)
                .map(|w| (w.id, w.x, w.y, w.w, w.h, w.mapped)).collect();
            crate::serial_println!("[xin] connection {ci}: {} B queued, {} windows, larger than 1x1 (id, x, y, w, h, mapped): {wins:x?}",
                c.outbuf.len(), c.windows.len());
        }
    }
}

/// The X keycode of a set-1 scancode: the evdev code + 8, which is what every X
/// client's keycode tables assume. The main block is scancode + 8; the extended
/// (`E0`) keys have codes of their own. 0 = a key this server does not know.
fn x_keycode(ext: bool, code: u8) -> u8 {
    if !ext {
        return code + 8;
    }
    match code {
        0x1C => 104, // KP_Enter
        0x1D => 105, // Control_R
        0x35 => 106, // KP_Divide
        0x38 => 108, // AltGr
        0x47 => 110, // Home
        0x48 => 111, // Up
        0x49 => 112, // Prior
        0x4B => 113, // Left
        0x4D => 114, // Right
        0x4F => 115, // End
        0x50 => 116, // Down
        0x51 => 117, // Next
        0x52 => 118, // Insert
        0x53 => 119, // Delete
        0x5B => 133, // Super_L
        0x5C => 134, // Super_R
        0x5D => 135, // Menu
        _ => 0,
    }
}

/// Pump REAL keyboard input into X key events. Pops PS/2 scancodes and delivers a
/// KeyPress(2)/KeyRelease(3) to every connection whose mapped window selected that
/// event (X keycode = scancode + 8). Called from the run_glibc wait loop while an X
/// client is up — this is how live hardware input reaches an X window (vs. the
/// injected self-test events). No-op (pops nothing) unless a window wants keys.

// ── Key to screen, measured in the kernel ([keylat]) ───────────────────────
//
// One key at a time (a typist, or the harness's perf step, waits for the
// character before the next key): when its make code entered the scancode
// ring, when the X server handed the KeyPress to a client, when that client
// read it, when the client next drew into a window, and when the desktop loop
// put that drawing on the screen. One line per key with the four gaps, and
// `keylat` in the shell gives the medians of the last 64 keys. Where the
// ~300 ms of chrome-2026-11b went (W23, task 4).
pub mod keylat {
    use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    const N: usize = 64;
    /// Stage reached by the key in flight: 0 none, 1 in the ring, 2 handed to a
    /// client, 3 read by it, 4 drawn.
    static STAGE: AtomicU64 = AtomicU64::new(0);
    static T: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];
    static SEQ: AtomicU64 = AtomicU64::new(0);
    static HIST: [[AtomicU64; 5]; N] = [const { [const { AtomicU64::new(0) }; 5] }; N];
    static HN: AtomicUsize = AtomicUsize::new(0);

    fn now() -> u64 {
        unsafe { core::arch::x86_64::_rdtsc() }
    }
    fn us(a: u64, b: u64) -> u64 {
        let per = crate::apic::TSC_PER_TICK.load(Ordering::Relaxed).max(1);
        b.saturating_sub(a) * 10_000 / per
    }
    fn at(stage: u64, from: u64) {
        if STAGE.compare_exchange(from, stage, Ordering::AcqRel, Ordering::Relaxed).is_ok() {
            T[stage as usize].store(now(), Ordering::Relaxed);
        }
    }
    /// A make code entered the scancode ring (any keyboard).
    pub fn key_in() {
        // A new key restarts the measurement, whatever stage the last one reached.
        STAGE.store(0, Ordering::Relaxed);
        at(1, 0);
    }
    /// The X server handed the KeyPress to a client.
    pub fn delivered() { at(2, 1) }
    /// A client read at least one event or reply.
    pub fn read() { at(3, 2) }
    /// A client drew into a window (the hosted window is now dirty).
    pub fn drawn() { at(4, 3) }
    /// The desktop loop put the hosted window on the screen.
    pub fn painted() {
        if STAGE.compare_exchange(4, 0, Ordering::AcqRel, Ordering::Relaxed).is_err() {
            return;
        }
        let t: [u64; 5] = core::array::from_fn(|i| T[i].load(Ordering::Relaxed));
        let p = now();
        let g = [us(t[1], t[2]), us(t[2], t[3]), us(t[3], t[4]), us(t[4], p), us(t[1], p)];
        let n = SEQ.fetch_add(1, Ordering::Relaxed) + 1;
        let slot = HN.fetch_add(1, Ordering::Relaxed) % N;
        for (i, v) in g.iter().enumerate() {
            HIST[slot][i].store(*v, Ordering::Relaxed);
        }
        crate::serial_println!(
            "[keylat] key {n}: ring to X client {} us, X queue to client read {} us, read to first draw {} us, draw to screen {} us; total {} us",
            g[0], g[1], g[2], g[3], g[4]);
    }
    /// `keylat`: medians and maxima of the last keys.
    pub fn summary() -> alloc::vec::Vec<alloc::string::String> {
        let n = HN.load(Ordering::Relaxed).min(N);
        if n == 0 {
            return alloc::vec![alloc::string::String::from("[keylat] no key measured yet"), wake_line()];
        }
        let names = ["ring to X client", "X queue to client read", "read to first draw", "draw to screen", "total"];
        let mut out = alloc::vec![alloc::format!("[keylat] last {n} keys (medians, max):")];
        for (i, name) in names.iter().enumerate() {
            let mut v: alloc::vec::Vec<u64> = (0..n).map(|k| HIST[k][i].load(Ordering::Relaxed)).collect();
            v.sort_unstable();
            out.push(alloc::format!("[keylat]   {name}: median {} us, max {} us", v[n / 2], v[n - 1]));
        }
        out.push(wake_line());
        out
    }
    /// How long woken tasks waited for the CPU (all tasks, since boot or "keylat reset").
    pub fn wake_line() -> alloc::string::String {
        let mut l = alloc::format!("[keylat] woken tasks waited for the CPU (ms; wake preemption, {} reschedule IPIs):",
            crate::sched::RESCHED_IPIS.load(Ordering::Relaxed));
        for (i, b) in crate::sched::WAKE_HIST.iter().enumerate() {
            l.push_str(&alloc::format!(" {} {}", crate::sched::WAKE_HIST_MS[i], b.load(Ordering::Relaxed)));
        }
        l
    }
    /// "keylat reset": forget the keys and the wake histogram (an A/B in one boot).
    pub fn reset() {
        HN.store(0, Ordering::Relaxed);
        STAGE.store(0, Ordering::Relaxed);
        for b in crate::sched::WAKE_HIST.iter() {
            b.store(0, Ordering::Relaxed);
        }
    }
}

pub fn pump_keyboard() {
    // IRQ-safe: called from task 0 (desktop) while the client's read/process spins on
    // XCONNS with IF=0 — a preemption holding this lock would deadlock (BUG-007 class).
    x86_64::instructions::interrupts::without_interrupts(|| {
        let wants = {
            let t = lock_home();
            t.iter().flatten().any(|c| c.windows.iter().any(|w| w.mapped && w.event_mask & 0x3 != 0))
        };
        if !wants {
            return;
        }
        while let Some(sc) = crate::ps2::poll_scancode() {
            // Raw scancode census (budget): the X event stream showed doubled makes
            // and NO breaks — decide whether the corruption is already in this ring
            // (HID->set1 translation) or in the event building below.
            {
                use core::sync::atomic::{AtomicU32, Ordering};
                static SC_DIAG: AtomicU32 = AtomicU32::new(64);
                if SC_DIAG.load(Ordering::Relaxed) > 0 {
                    SC_DIAG.fetch_sub(1, Ordering::Relaxed);
                    crate::serial_println!("[sc] {sc:#04x}");
                }
            }
            // An extended key is two bytes, `E0` and the code, and they can arrive
            // in different calls. Taking `E0` for a key (code 0x60, a release) and
            // the code after it for a plain key turned every arrow into a keypad
            // digit and AltGr into Alt.
            if sc == 0xE0 {
                KBD_EXT.store(true, core::sync::atomic::Ordering::Relaxed);
                continue;
            }
            let ext = KBD_EXT.swap(false, core::sync::atomic::Ordering::Relaxed);
            let pressed = sc & 0x80 == 0;
            let code = sc & 0x7f;
            // Track the modifiers ourselves: the state field of EVERY event carries
            // them, and a key event only means "A" instead of "a" because of it.
            let bit: u16 = match (ext, code) {
                (false, 0x2a) | (false, 0x36) => 0x1, // Shift_L / Shift_R
                (_, 0x1d) => 0x4,                     // Control_L / Control_R
                (false, 0x38) => 0x8,                 // Alt_L (Mod1)
                (true, 0x38) => 0x80,                 // AltGr = Mode_switch (Mod5)
                _ => 0,
            };
            if bit != 0 {
                if pressed {
                    MOD_STATE.fetch_or(bit, core::sync::atomic::Ordering::Relaxed);
                } else {
                    MOD_STATE.fetch_and(!bit, core::sync::atomic::Ordering::Relaxed);
                }
            }
            // Caps Lock toggles the Lock bit on its press.
            if !ext && code == 0x3a && pressed {
                MOD_STATE.fetch_xor(0x2, core::sync::atomic::Ordering::Relaxed);
            }
            let keycode = x_keycode(ext, code);
            trace(format_args!("key sc={sc:#04x} ext={ext} -> keycode {keycode} {} state={:#x}",
                if pressed { "press" } else { "release" }, MOD_STATE.load(core::sync::atomic::Ordering::Relaxed)));
            if keycode == 0 {
                continue;
            }
            let want: u32 = if pressed { 0x1 } else { 0x2 }; // KeyPress / KeyRelease mask
            let kind: u8 = if pressed { 2 } else { 3 };
            if pressed {
                keylat::delivered();
            }
            // Ferry typed characters to the page over the CDP bridge as well (the
            // reliable route; see pump_mouse). Presses only; layout = our qwerty
            // keysym table, shift honoured. Enter goes as a raw key event — an
            // inserted "\r" does not submit forms or activate links.
            if pressed && crate::ring3::cdp_ferry() {
                let (lo, hi) = keysym_for(keycode);
                let shifted = MOD_STATE.load(core::sync::atomic::Ordering::Relaxed) & 0x1 != 0;
                let ks = if shifted { hi } else { lo };
                if (0x20..0x7f).contains(&ks) {
                    let mut b = [0u8; 4];
                    crate::ring3::cdp_input_text(char::from_u32(ks).unwrap_or(' ').encode_utf8(&mut b));
                } else if ks == 0xff0d {
                    crate::ring3::cdp_input_enter();
                }
            }
            let (mx, my) = crate::mouse::pos();
            #[allow(unused_assignments)]
            let mut t = lock_home();
            // A key belongs to ONE window: the one holding the input focus, else the
            // one on top of the screen. Broadcasting a keystroke to every window was
            // survivable with a single demo client and is not, once a browser has a
            // toolbar, a page and a dialog open at the same time.
            let focus = FOCUS_WINDOW.load(core::sync::atomic::Ordering::Relaxed);
            let chosen = if focus != 0 { Some(focus) } else { topmost_presented() };
            // Multicast to the selecting connections first (real X semantics: the
            // event connection selected keys on a window another connection owns).
            if let Some(wid) = chosen {
                drop(t);
                if deliver_selected(wid, want, kind, keycode, mx as i16, my as i16,
                                    mx as i16, my as i16) > 0 {
                    continue;
                }
                t = lock_home();
            }
            if let Some(ci) = chosen.and_then(|w| conn_of_window(&t[..], w)) {
                let wid = chosen.unwrap();
                if let Some(conn) = t[ci].as_mut() {
                    let (wx, wy) = conn.windows.iter().find(|w| w.id == wid)
                        .map(|w| (w.x, w.y)).unwrap_or((0, 0));
                    send_input(conn, kind, keycode, wid, mx as i16, my as i16,
                               mx as i16 - wx, my as i16 - wy);
                }
            } else {
                for conn in t.iter_mut().flatten() {
                    if let Some((wid, wx, wy)) = input_target(conn, want) {
                        send_input(conn, kind, keycode, wid, mx as i16, my as i16,
                                   mx as i16 - wx, my as i16 - wy);
                    }
                }
            }
        }
    });
}

/// Pump REAL mouse input into X pointer events: button press AND release on the real
/// button edges, motion while it moves, and an EnterNotify when the pointer crosses
/// into another window. Called from the run_glibc wait loop.
///
/// The button used to arrive as a one-shot "press latch" with no release at all — a
/// toolkit that never sees ButtonRelease believes the button is still held, so the
/// next press reads as a drag and no click ever completes. Both edges are read from
/// the driver's live button state instead.
static LAST_MOUSE: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(u32::MAX);
static LAST_BTN: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static POINTER_WIN: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
/// Tick of the last stall dump (see send_input); dumps re-fire after a cooldown.
static STALL_LAST_DUMP: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// PER-CLIENT event selections: (window id, connection index, event mask). Real X
/// keeps a mask per client per window — every client selects independently, and an
/// event goes to EVERY selecting client on ITS OWN connection. Our single per-window
/// mask meant "last writer wins": chrome's fourth connection cleared the mask the
/// event connection had set, and input went to the window's OWNER (fd 604) while
/// the thread that actually reads events sat on fd 603 forever.
static SELECTIONS: Mutex<Vec<(u32, usize, u32)>> = Mutex::new(Vec::new());

/// This connection's index in XCONNS, recoverable while it is OUT of the table
/// (the processing dance): rid_base = (index+1)<<21.
fn conn_index(c: &XConn) -> usize {
    // rid_base = RID_BASE + index * 0x200000 — subtract the BASE first. The naive
    // (rid>>21)-1 pointed every selection one slot too high: the browser window
    // (slot 0, the fd main actually reads) registered under slot 1, and the
    // multicast delivered every click to the wrong neighbour.
    ((c.rid_base.wrapping_sub(RID_BASE) >> 21) as usize).min(MAX_XCONN - 1)
}

fn select_events(win: u32, conn_idx: usize, mask: u32) {
    let mut t = SELECTIONS.lock();
    match t.iter_mut().find(|(w, c, _)| *w == win && *c == conn_idx) {
        Some(e) => e.2 = mask,
        None => t.push((win, conn_idx, mask)),
    }
}

/// Deliver one input event to EVERY connection that selected `want` on `win`,
/// each at window-local (lx,ly) with root (rx,ry). `want`==0 delivers to every
/// selector of anything. The OWNING connection's fallback stays for clients that
/// never registered a selection (our own demo apps).
fn deliver_selected(win: u32, want: u32, kind: u8, detail: u8, rx: i16, ry: i16, lx: i16, ly: i16) -> u32 {
    // First ButtonPress: dump main's recent syscall ring. In the no-dialog runs
    // main sits in a ~1.3 s nanosleep cycle and never polls its X connection; the
    // ring names what else that loop does — the difference between "glib pump
    // throttled" and "main stuck in some wait chain" in one boot.
    if kind == 4 {
        static ONCE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
        if !ONCE.swap(true, core::sync::atomic::Ordering::Relaxed) {
            crate::ring3::dump_main_syscalls();
            // The window world at the first click, once. This is the path the
            // desktop's hosted-frame click takes (deliver_button), so the census
            // lives here and not only in deliver_pointer. Each lock in its own
            // statement; the delivery below takes them again.
            let presented: Vec<_> = PRESENTED.lock().clone();
            crate::serial_println!("[xin] census at ButtonPress win={win:#x} local({lx},{ly}) root({rx},{ry}): retained={:#x} presented(id,dx,dy,sc,w,h,ord)={presented:?}",
                RETAINED_ID.load(core::sync::atomic::Ordering::Relaxed));
            let sel: Vec<_> = SELECTIONS.lock().clone();
            crate::serial_println!("[xin]   selections(win,conn,mask)={sel:?}");
            let readers: Vec<_> = READER_CONNS.lock().clone();
            crate::serial_println!("[xin]   readers={readers:?} focus={:#x}", FOCUS_WINDOW.load(core::sync::atomic::Ordering::Relaxed));
            let t = lock_home();
            for (ci, c) in t.iter().enumerate() {
                if let Some(c) = c {
                    let wins: Vec<_> = c.windows.iter()
                        .map(|w| (w.id, w.x, w.y, w.w, w.h, w.mapped, w.event_mask)).collect();
                    crate::serial_println!("[xin]   conn {ci}: queued={} B windows(id,x,y,w,h,mapped,mask)={wins:?}", c.outbuf.len());
                }
            }
        }
    }
    // Every connection that selected `want` on `win`...
    let targets: Vec<usize> = SELECTIONS.lock().iter()
        .filter(|(w, _, m)| *w == win && (want == 0 || m & want != 0))
        .map(|(_, c, _)| *c)
        .collect();
    // Only the selectors. Until BUG-090 the browser's first connection (the one
    // that selects input on its window) was dead to xcb, and a copy of every event
    // went to each connection that had ever read anything; with that connection
    // alive a copy is a second key press.
    let mut sent = 0;
    let mut t = lock_home();
    for ci in targets {
        if let Some(Some(conn)) = t.get_mut(ci).map(|s| s.as_mut()) {
            // If this connection does NOT own `win`, its xcb event demux has no
            // handler for that id and drops the event (measured: fd606 read every
            // click and did nothing). Deliver instead for a MAPPED toplevel THIS
            // connection owns and selected pointer input on — chrome's Ozone input
            // window. The click's window-local coords are kept; that window is the
            // one chrome's event source is actually listening for.
            let owns = conn.windows.iter().any(|w| w.id == win && w.mapped);
            let ev_win = if owns {
                win
            } else {
                match conn.windows.iter()
                    .filter(|w| w.mapped && w.event_mask & (kind_mask(kind)) != 0)
                    .map(|w| w.id).next()
                {
                    Some(w) => w,
                    None => win, // no better target: send as-is
                }
            };
            send_input(conn, kind, detail, ev_win, rx, ry, lx, ly);
            sent += 1;
        }
    }
    sent
}

/// The event-mask bit an input event of `kind` is selected by (for choosing a
/// delivery window on a foreign connection).
fn kind_mask(kind: u8) -> u32 {
    match kind {
        2 | 3 => 0x3,   // KeyPress/Release
        4 => 0x4,       // ButtonPress
        5 => 0x8,       // ButtonRelease
        6 => 0x40,      // PointerMotion
        7 | 8 => 0x10,  // Enter/Leave
        _ => 0,
    }
}
/// The window whose pixels the desktop currently shows in its frame (windowed mode).
static RETAINED_ID: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
/// The window that holds the keyboard focus (SetInputFocus), 0 = none yet.
static FOCUS_WINDOW: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// WHERE each window's pixels last landed on the screen: (window id, dx, dy, scale,
/// width, height, present order). The server blits a window centred and integer-scaled
/// (a small dialog is magnified 4x), so the pointer position on screen says nothing
/// about the window coordinate until it is run back through that same transform. The
/// newest entry that contains the pointer is the one on top — which is exactly what a
/// person sees, and therefore what they mean to click.
static PRESENTED: Mutex<Vec<(u32, i32, i32, i32, i32, i32, u64)>> = Mutex::new(Vec::new());
pub static PRESENT_ORDER_COUNT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static PRESENT_ORDER: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Record a window's screen placement after a fullscreen present.
fn note_presented(id: u32, w: u16, h: u16, dx: i64, dy: i64, sc: usize) {
    let (dx, dy, sc) = (dx, dy, sc as i64);
    let n = PRESENT_ORDER.fetch_add(1, core::sync::atomic::Ordering::Relaxed) + 1;
    PRESENT_ORDER_COUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    let mut t = PRESENTED.lock();
    let e = (id, dx as i32, dy as i32, sc as i32, w as i32, h as i32, n);
    match t.iter_mut().find(|r| r.0 == id) {
        Some(r) => *r = e,
        None => t.push(e),
    }
}

/// The window under a screen position, and the transform back into it: (id, dx, dy,
/// scale). Topmost (most recently presented) first. None if the pointer is over bare
/// desktop.
fn window_at(px: i32, py: i32) -> Option<(u32, i32, i32, i32)> {
    // Same rule as in pump_mouse (BUG-320): the lock is held with interrupts off.
    x86_64::instructions::interrupts::without_interrupts(|| {
        let t = PRESENTED.lock();
        note_lock_held();
        t.iter()
            .filter(|(_, dx, dy, sc, w, h, _)| {
                px >= *dx && px < dx + w * sc && py >= *dy && py < dy + h * sc
            })
            .max_by_key(|r| r.6)
            .map(|(id, dx, dy, sc, _, _, _)| (*id, *dx, *dy, *sc))
    })
}

/// The window on top of the screen right now (the most recently presented one).
fn topmost_presented() -> Option<u32> {
    PRESENTED.lock().iter().max_by_key(|r| r.6).map(|r| r.0)
}

/// The connection index owning a window id, if it is mapped.
fn conn_of_window(t: &[Option<XConn>], id: u32) -> Option<usize> {
    t.iter().position(|c| {
        c.as_ref().map_or(false, |c| c.windows.iter().any(|w| w.id == id && w.mapped))
    })
}
/// BUG-320 (self-test): how often `pump_mouse` / `window_at` held an X server lock with
/// interrupts ENABLED. The X client's read/process path takes the same locks with IF=0;
/// task 0 (the desktop loop), preempted while it holds one of them, leaves the client
/// spinning on it for good (single CPU: nobody else runs to release it).
static LOCKED_WITH_IF: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

fn note_lock_held() {
    if cfg!(feature = "selftest") && x86_64::instructions::interrupts::are_enabled() {
        LOCKED_WITH_IF.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    }
}

/// Boot self-test `[xlock]` (BUG-320): the desktop loop's mouse path takes the X server
/// locks with interrupts off.
pub fn xlock_selftest() {
    if !x86_64::instructions::interrupts::are_enabled() {
        crate::serial_println!("[xlock] interrupts are off here: not run");
        return;
    }
    let before = LOCKED_WITH_IF.load(core::sync::atomic::Ordering::Relaxed);
    pump_mouse();
    let _ = window_at(0, 0);
    let n = LOCKED_WITH_IF.load(core::sync::atomic::Ordering::Relaxed) - before;
    crate::serial_println!(
        "[xlock] pump_mouse and window_at, run with interrupts on: X locks taken with interrupts enabled = {n} (want 0) -> {}",
        if n == 0 { "OK (a preemption cannot catch task 0 holding them) ✓" } else { "FAILED" }
    );
}

pub fn pump_mouse() {
    // ButtonPressMask(0x4) or PointerMotionMask(0x40) selected by any mapped window?
    // Interrupts off while XCONNS is held (BUG-320): task 0 is the desktop loop, and a
    // timer tick that preempts it inside this scan leaves an X client that reads or
    // processes a request with IF=0 spinning on the lock for good.
    let wants = x86_64::instructions::interrupts::without_interrupts(|| {
        let t = XCONNS.lock();
        note_lock_held();
        t.iter().flatten().any(|c| c.windows.iter().any(|w| w.mapped && w.event_mask & 0x44 != 0))
    });
    if !wants {
        // Nothing to deliver to — but the button queue still has to be emptied, or the
        // clicks a person made on the DESKTOP would be waiting in it and all arrive at
        // once the moment an X window opens.
        while crate::mouse::take_button_event().is_some() {}
        return;
    }
    let (px, py) = crate::mouse::pos();
    // Button EDGES, drained from the driver's queue. Sampling the button level here
    // loses a whole click whenever this loop does not happen to run during the ~100 ms
    // the button is down — and while a browser has the CPU, that is most clicks.
    // The legacy press latch is drained too so it cannot fire a second copy.
    let _ = crate::mouse::take_press();
    while let Some((down, cx, cy)) = crate::mouse::take_button_event() {
        LAST_BTN.store(down, core::sync::atomic::Ordering::Relaxed);
        deliver_pointer(if down { 4 } else { 5 }, 1, cx, cy, 0);
        // ALSO ferry the click to the page over the CDP bridge. Coordinates:
        // screen -> window via the presented transform, then minus the browser
        // chrome above the viewport (tab strip + omnibox + infobar ≈ 143 px in
        // the current 800x600 layout).
        if let Some((_, dx, dy, sc)) = window_at(cx as i32, cy as i32) {
            if sc > 0 {
                let wx = (cx as i32 - dx) / sc;
                let wy = (cy as i32 - dy) / sc;
                let (vx, vy) = crate::ring3::cdp_window_to_view(wx.max(0), wy);
                crate::ring3::cdp_input_mouse(if down { 4 } else { 5 }, vx, vy);
            }
        }
    }
    // Cursor moved -> MotionNotify(6), preceded by an EnterNotify(7) when the pointer
    // crosses into a different window (chrome starts hover tracking on the crossing).
    let packed = ((px as u32 & 0xffff) << 16) | (py as u32 & 0xffff);
    if LAST_MOUSE.swap(packed, core::sync::atomic::Ordering::Relaxed) != packed {
        let now = window_at(px as i32, py as i32).map(|t| t.0).unwrap_or(0);
        if POINTER_WIN.swap(now, core::sync::atomic::Ordering::Relaxed) != now && now != 0 {
            deliver_pointer(7, 0, px, py, 0);
        }
        deliver_pointer(6, 0, px, py, 0x40);
    }
}

/// Deliver one pointer event at a SCREEN position to the window that is actually under
/// it — found through the presentation table, so the window a person sees at that spot
/// is the window that gets the event, at the coordinate it sees. `require_mask` is a
/// bit the target must have selected (0 = deliver regardless: a click is never worth
/// dropping over a mask, a motion often is).
///
/// Fallback, when nothing has been presented yet (or the desktop composites the app in
/// windowed mode): the largest mapped window, as before.
fn deliver_pointer(kind: u8, detail: u8, px: usize, py: usize, require_mask: u32) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        // First ButtonPress: a census of the X window world, once. Which windows
        // have presented pixels and where, which window the hit test picks, who
        // selected what on it, and which connections are readers. Each lock is
        // taken and dropped in its own statement (deliver_selected takes them
        // again below).
        if kind == 4 {
            static ONCE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
            if !ONCE.swap(true, core::sync::atomic::Ordering::Relaxed) {
                let presented: Vec<_> = PRESENTED.lock().clone();
                crate::serial_println!("[xin] census at ButtonPress ({px},{py}): presented(id,dx,dy,sc,w,h,ord)={presented:?}");
                let hit = window_at(px as i32, py as i32);
                crate::serial_println!("[xin]   window_at -> {hit:?}");
                let sel: Vec<_> = SELECTIONS.lock().clone();
                crate::serial_println!("[xin]   selections(win,conn,mask)={sel:?}");
                let readers: Vec<_> = READER_CONNS.lock().clone();
                crate::serial_println!("[xin]   readers={readers:?}");
                let t = lock_home();
                for (ci, c) in t.iter().enumerate() {
                    if let Some(c) = c {
                        let wins: Vec<_> = c.windows.iter().filter(|w| w.mapped)
                            .map(|w| (w.id, w.x, w.y, w.w, w.h, w.event_mask)).collect();
                        crate::serial_println!("[xin]   conn {ci}: queued={} B mapped(id,x,y,w,h,mask)={wins:?}", c.outbuf.len(), );
                    }
                }
            }
        }
        if let Some((wid, dx, dy, sc)) = window_at(px as i32, py as i32) {
            let lx = ((px as i32 - dx) / sc.max(1)) as i16;
            let ly = ((py as i32 - dy) / sc.max(1)) as i16;
            // Real X: the event goes to EVERY connection that selected it on this
            // window — chrome selects on its event connection, not on the window's
            // owner. Fall back to the owner only when nobody registered a selection.
            let sent = deliver_selected(wid, require_mask, kind, detail,
                                        px as i16, py as i16, lx, ly);
            if sent > 0 {
                return;
            }
            let mut t = lock_home();
            if let Some(ci) = conn_of_window(&t[..], wid) {
                if let Some(conn) = t[ci].as_mut() {
                    let ok = conn.windows.iter().find(|w| w.id == wid)
                        .map(|w| require_mask == 0 || w.event_mask & require_mask != 0)
                        .unwrap_or(false);
                    if ok {
                        send_input(conn, kind, detail, wid, px as i16, py as i16, lx, ly);
                    }
                }
            }
            return;
        }
        let mut t = lock_home();
        for conn in t.iter_mut().flatten() {
            if let Some((wid, wx, wy)) = input_target(conn, require_mask) {
                send_input(conn, kind, detail, wid, px as i16, py as i16,
                           px as i16 - wx, py as i16 - wy);
            }
        }
    });
}

/// Deliver a click (ButtonPress + ButtonRelease, button 1) to the front mapped window
/// at WINDOW-LOCAL coordinates — used by the desktop to route a click on a hosted X
/// app's framed window to the app (so its GTK button activates). IRQ-safe: task 0 must
/// not hold XCONNS across a preemption while the IF=0 client read/process spins on it.
/// Deliver FocusIn(9) / FocusOut(10) to the front mapped window. GTK/GDK only routes
/// key events to widgets once its window has keyboard focus (a FocusIn), so the desktop
/// sends this when the hosted X window gains/loses focus. IRQ-safe.
pub fn deliver_focus(focused: bool) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut t = lock_home();
        for conn in t.iter_mut().flatten() {
            if let Some(wid) = conn.windows.iter().filter(|w| w.mapped && w.w > 1 && w.h > 1)
                .max_by_key(|w| w.w as u32 * w.h as u32).map(|w| w.id)
            {
                let mut e = [0u8; 32];
                e[0] = if focused { 9 } else { 10 }; // FocusIn / FocusOut
                e[1] = 0; // detail = NotifyAncestor
                e[2..4].copy_from_slice(&conn.seq.to_le_bytes());
                e[4..8].copy_from_slice(&wid.to_le_bytes()); // event window
                // mode@8 = 0 (NotifyNormal); rest unused
                conn.outbuf.extend_from_slice(&e);
                trace(format_args!("deliver_focus({focused}) -> win {wid:#x}"));
            }
        }
    });
}

pub fn deliver_button(lx: i16, ly: i16) {
    // Move the pointer there FIRST: chrome (like GTK) tracks the pointer from motion
    // events and hit-tests the press against what it believes is under the cursor.
    deliver_to_shown(6, 0, lx, ly);
    deliver_to_shown(4, 1, lx, ly);
    deliver_to_shown(5, 1, lx, ly);
    // Desktop-hosted chrome: ferry the click over the CDP bridge as well, exactly
    // like pump_mouse does in the boot phase — the reliable route into the page.
    // Coordinates are already window-local here; only the browser chrome above the
    // viewport (~143 px) is subtracted.
    let (vx, vy) = crate::ring3::cdp_window_to_view(lx as i32, ly as i32);
    crate::ring3::cdp_input_mouse(4, vx, vy);
    crate::ring3::cdp_input_mouse(5, vx, vy);
    trace(format_args!("deliver_button local=({lx},{ly})"));
}

/// A mouse button went DOWN over the hosted window, at window-local coordinates:
/// the pointer is moved there first, then the ButtonPress. `button`: 1 left,
/// 2 middle, 3 right. The release comes separately ([`hosted_release`]), when the
/// physical button goes up, so a drag (text selection, a scrollbar, a slider) is a
/// press, motion with the button in the state, and a release, as on any X server.
pub fn hosted_press(button: u8, lx: i16, ly: i16) {
    deliver_to_shown(6, 0, lx, ly);
    deliver_to_shown(4, button, lx, ly);
}

/// The button went up again; the pointer may be outside the window by now
/// (coordinates beyond its edges are what X reports during a grab).
pub fn hosted_release(button: u8, lx: i16, ly: i16) {
    deliver_to_shown(5, button, lx, ly);
}

/// The mouse wheel over the hosted window: X has no wheel event, a notch is a
/// press and release of button 4 (up) or 5 (down). `notches` > 0 = scroll down.
pub fn hosted_wheel(notches: i32, lx: i16, ly: i16) {
    let button = if notches > 0 { 5 } else { 4 };
    for _ in 0..notches.unsigned_abs().min(32) {
        deliver_to_shown(4, button, lx, ly);
        deliver_to_shown(5, button, lx, ly);
    }
}

/// Pointer motion from the desktop into the hosted window (hover). Same routing as the
/// click, so whatever highlights under the cursor is what a click would actually hit.
pub fn deliver_motion(lx: i16, ly: i16) {
    deliver_to_shown(6, 0, lx, ly);
}

/// Send one event, at window-local coordinates, to the window the desktop is SHOWING
/// (the retained one) — falling back to the largest mapped window of each connection
/// when nothing has been retained yet.
fn deliver_to_shown(kind: u8, detail: u8, lx: i16, ly: i16) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut t = lock_home();
        let shown = RETAINED_ID.load(core::sync::atomic::Ordering::Relaxed);
        if shown != 0 {
            if let Some(ci) = conn_of_window(&t[..], shown) {
                if let Some(conn) = t[ci].as_mut() {
                    let (mx, my) = conn.windows.iter().find(|w| w.id == shown)
                        .map(|w| (w.x as i32, w.y as i32)).unwrap_or((0, 0));
                    let (rx, ry) = (lx as i32 + mx, ly as i32 + my);
                    // The window under the pointer: the newest popup that contains
                    // it, else the main window. While a button is held the events
                    // stay with the window the press went to (X's implicit grab),
                    // so a drag that crosses a popup or leaves the window is whole.
                    let held = IMPLICIT_GRAB.load(core::sync::atomic::Ordering::Relaxed);
                    let target = conn.windows.iter().rev()
                        .filter(|w| w.mapped && w.w > 1 && w.h > 1)
                        .find(|w| if held != 0 { w.id == held } else {
                            w.id != shown && rx >= w.x as i32 && ry >= w.y as i32
                                && rx < w.x as i32 + w.w as i32 && ry < w.y as i32 + w.h as i32
                        })
                        .map(|w| (w.id, w.x as i32, w.y as i32))
                        .unwrap_or((shown, mx, my));
                    if (1..=3).contains(&detail) {
                        if kind == 4 && held == 0 {
                            IMPLICIT_GRAB.store(target.0, core::sync::atomic::Ordering::Relaxed);
                        } else if kind == 5 && crate::mouse::buttons() == 0 {
                            IMPLICIT_GRAB.store(0, core::sync::atomic::Ordering::Relaxed);
                        }
                    }
                    LAST_ROOT.store(((rx as i16 as u16 as u32) << 16) | ry as i16 as u16 as u32,
                        core::sync::atomic::Ordering::Relaxed);
                    send_input(conn, kind, detail, target.0, rx as i16, ry as i16,
                               (rx - target.1) as i16, (ry - target.2) as i16);
                    return;
                }
            }
        }
        for conn in t.iter_mut().flatten() {
            // The toolkit routes events to its client-side child widgets itself, so
            // deliver to the largest mapped window (the toplevel) — no mask required.
            if let Some((wid, wx, wy)) = input_target(conn, 0) {
                send_input(conn, kind, detail, wid, lx + wx, ly + wy, lx, ly);
            }
        }
    });
}

/// The keysym of a character: Latin-1 is its own keysym, the euro sign has a
/// legacy one, everything else is the Unicode keysym (0x01000000 + code point).
fn char_keysym(c: char) -> u32 {
    match c as u32 {
        0x20..=0x7e | 0xa0..=0xff => c as u32,
        0x20ac => 0x20ac,
        u => 0x0100_0000 | u,
    }
}

/// The four keysyms of a keycode, from the ACTIVE keyboard layout (ps2::layout,
/// the one the rest of the system types with): group 1 = (plain, Shift), group 2
/// = (AltGr, AltGr+Shift). AltGr is Mode_switch, which is how the core protocol
/// selects the second group. NoSymbol (0) where a level has nothing.
fn keysyms4(kc: u8) -> [u32; 4] {
    let both = |k: u32| [k, k, 0, 0];
    match kc {
        9 => both(0xff1b),            // Escape
        22 => both(0xff08),           // BackSpace
        23 => [0xff09, 0xfe20, 0, 0], // Tab, ISO_Left_Tab
        36 => both(0xff0d),           // Return
        37 => both(0xffe3),           // Control_L
        50 => both(0xffe1),           // Shift_L
        62 => both(0xffe2),           // Shift_R
        64 => both(0xffe9),           // Alt_L
        65 => both(0x20),             // space
        66 => both(0xffe5),           // Caps_Lock
        67..=76 => both(0xffbe + (kc as u32 - 67)), // F1..F10
        77 => both(0xff7f),           // Num_Lock
        95 => both(0xffc8),           // F11
        96 => both(0xffc9),           // F12
        104 => both(0xff8d),          // KP_Enter
        105 => both(0xffe4),          // Control_R
        106 => both(0xffaf),          // KP_Divide
        108 => both(0xff7e),          // Mode_switch (AltGr)
        110 => both(0xff50),          // Home
        111 => both(0xff52),          // Up
        112 => both(0xff55),          // Prior
        113 => both(0xff51),          // Left
        114 => both(0xff53),          // Right
        115 => both(0xff57),          // End
        116 => both(0xff54),          // Down
        117 => both(0xff56),          // Next
        118 => both(0xff63),          // Insert
        119 => both(0xffff),          // Delete
        133 => both(0xffeb),          // Super_L
        134 => both(0xffec),          // Super_R
        135 => both(0xff67),          // Menu
        10..=94 => {
            let sc = kc - 8;
            let layout = crate::ps2::layout();
            let ks = |shift: bool| eurokeymap::translate(layout, sc, shift)
                .filter(|c| !c.is_control()).map(char_keysym).unwrap_or(0);
            let l3 = eurokeymap::level3(layout, sc).map(char_keysym).unwrap_or(0);
            [ks(false), ks(true), l3, l3]
        }
        _ => [0; 4],
    }
}

/// (plain, shifted) keysym of a keycode.
fn keysym_for(kc: u8) -> (u32, u32) {
    let k = keysyms4(kc);
    (k[0], k[1])
}

fn trace(args: core::fmt::Arguments) {
    if TRACE.load(core::sync::atomic::Ordering::Relaxed) {
        crate::serial_println!("[xserver] {args}");
    }
}

// ── Protocol ────────────────────────────────────────────────────────────────

/// Request/PutImage counters + cycles inside process() — the X half of the ledger.
pub static REQ_COUNT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
pub static PUTIMAGE_COUNT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
pub static PUTIMAGE_BYTES: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
pub static PROCESS_CYCLES: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

fn process(c: &mut XConn) {
    let t0 = unsafe { core::arch::x86_64::_rdtsc() };
    process_inner(c);
    PROCESS_CYCLES.fetch_add(unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(t0),
        core::sync::atomic::Ordering::Relaxed);
}

fn process_inner(c: &mut XConn) {
    if c.state == State::PreSetup {
        // Setup request: 12-byte header + auth-name(pad4) + auth-data(pad4).
        if c.inbuf.len() < 12 {
            return;
        }
        c.swap = c.inbuf[0] == b'B'; // 'l' little (0x6c), 'B' big (0x42)
        let n = rd16(c, 6) as usize; // auth-protocol-name length
        let d = rd16(c, 8) as usize; // auth-protocol-data length
        let total = 12 + pad4(n) + pad4(d);
        if c.inbuf.len() < total {
            return; // wait for the rest
        }
        c.inbuf.drain(0..total);
        let reply = setup_reply(c.swap, c.rid_base);
        c.outbuf.extend_from_slice(&reply);
        c.state = State::Connected;
        trace(format_args!("setup: swap={} -> {}-byte reply, CONNECTED", c.swap, reply.len()));
        // fall through: the client may have pipelined requests already
    }
    // Connected: consume whole requests (each: 4-byte header, length in 4-byte units).
    while c.state == State::Connected && c.inbuf.len() >= 4 {
        let len_units = rd16(c, 2) as usize;
        let req_len = if len_units == 0 { 4 } else { len_units * 4 }; // BIG-REQUESTS not enabled
        if req_len < 4 || c.inbuf.len() < req_len {
            break; // incomplete request
        }
        let opcode = c.inbuf[0];
        let detail = c.inbuf[1];
        REQ_COUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        if c.inbuf[0] == 72 {
            PUTIMAGE_COUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            PUTIMAGE_BYTES.fetch_add(req_len as u64, core::sync::atomic::Ordering::Relaxed);
        }
        let req: Vec<u8> = c.inbuf.drain(0..req_len).collect();
        c.seq = c.seq.wrapping_add(1);
        let out_before = c.outbuf.len();
        handle_request(c, opcode, detail, &req);
        // Reply-length audit: for a REPLY (type 1) verify actual bytes == 32 + length*4.
        // A mismatch corrupts Xlib's stream (the class of bug that broke SDL/GTK).
        if TRACE.load(core::sync::atomic::Ordering::Relaxed) {
            let added = c.outbuf.len() - out_before;
            if added >= 8 && c.outbuf[out_before] == 1 {
                let declared = u32::from_le_bytes([
                    c.outbuf[out_before + 4], c.outbuf[out_before + 5],
                    c.outbuf[out_before + 6], c.outbuf[out_before + 7],
                ]) as usize;
                let expect = 32 + declared * 4;
                if added != expect {
                    crate::serial_println!("[xserver] !! op={opcode} REPLY LEN MISMATCH: sent {added} B, declared {declared} units -> expect {expect} B");
                } else {
                    trace(format_args!("reply op={opcode} ok ({added} B, {declared} units)"));
                }
            }
        }
    }
}

fn handle_request(c: &mut XConn, opcode: u8, detail: u8, req: &[u8]) {
    trace(format_args!("req op={opcode} detail={detail} len={} seq={}", req.len(), c.seq));
    match opcode {
        // QueryExtension (98): report every extension as not-present so Xlib falls
        // back to core protocol. Reply: present=0.
        98 => {
            // QueryExtension(name): the name is a length-prefixed string at req[4..].
            // Trace it — whether chrome asks for XInputExtension (and thus wants XI2
            // GenericEvents instead of core ButtonPress) is the whole question about
            // why it reads our click and does nothing.
            let nlen = ru16(c, req, 4) as usize;
            let mut xfixes = false;
            if req.len() >= 8 + nlen {
                let name = alloc::string::String::from_utf8_lossy(&req[8..8 + nlen]);
                xfixes = name == "XFIXES" && DESKTOP_CLIENT.load(core::sync::atomic::Ordering::Relaxed);
                trace(format_args!("QueryExtension {:?} -> {}", name, if xfixes { "present" } else { "not present" }));
            }
            let mut r = reply_header(c, 0);
            // 24 extra bytes already zero: present(1)=0, major(1), first-event(1),
            // first-error(1), then 20 unused.
            if xfixes {
                r[8] = 1;
                r[9] = XFIXES_OPCODE;
                r[10] = XFIXES_EVENT;
                r[11] = XFIXES_ERROR;
            }
            pad_reply(&mut r);
            c.outbuf.extend_from_slice(&r);
        }
        // XFIXES, as far as a client needs it to learn that a selection changed
        // hands: QueryVersion (minor 0) and SelectSelectionInput (minor 2). The
        // other requests a toolkit sends (cursor hide/show, regions) owe no
        // reply and are ignored; the four that do get BadImplementation.
        XFIXES_OPCODE if DESKTOP_CLIENT.load(core::sync::atomic::Ordering::Relaxed) => match detail {
            0 => {
                let major = ru32(c, req, 4).min(5);
                let mut r = reply_header(c, 0);
                wr32(c, &mut r, 8, major);
                wr32(c, &mut r, 12, 0);
                c.outbuf.extend_from_slice(&r);
            }
            2 => {
                let win = ru32(c, req, 4);
                let sel = ru32(c, req, 8);
                let mask = ru32(c, req, 12);
                let me = conn_index(c);
                let mut t = XFIXES_SELECT.lock();
                t.retain(|e| !(e.0 == me && e.1 == win && e.2 == sel));
                if mask != 0 {
                    t.push((me, win, sel, mask));
                }
                trace(format_args!("XFixesSelectSelectionInput window={win:#x} selection={sel} mask={mask:#x}"));
            }
            4 | 19 | 24 | 25 | 34 => send_error(c, 17, 0, opcode, detail as u16),
            _ => {}
        },
        // ChangeProperty (18) / DeleteProperty (19): properties are not stored, but a
        // client that selected PropertyChange on the window is told, with the server
        // time. That event is how a toolkit ASKS for the server time (a no-op
        // property change, then it waits for the notify); without it chrome stamps
        // its selections with CurrentTime.
        18 | 19 if DESKTOP_CLIENT.load(core::sync::atomic::Ordering::Relaxed) => {
            let win = ru32(c, req, 4);
            let atom = ru32(c, req, 8);
            let me = conn_index(c);
            let wants = SELECTIONS.lock().iter().any(|(w, ci, m)| *w == win && *ci == me && m & 0x0040_0000 != 0)
                || c.windows.iter().any(|w| w.id == win && w.event_mask & 0x0040_0000 != 0);
            if wants {
                let mut e = [0u8; 32];
                e[0] = 28; // PropertyNotify
                e[2..4].copy_from_slice(&c.seq.to_le_bytes());
                e[4..8].copy_from_slice(&win.to_le_bytes());
                e[8..12].copy_from_slice(&atom.to_le_bytes());
                e[12..16].copy_from_slice(&((crate::interrupts::ticks() * 10) as u32).to_le_bytes());
                e[16] = if opcode == 19 { 1 } else { 0 }; // Deleted / NewValue
                c.outbuf.extend_from_slice(&e);
            }
        }
        // GetInputFocus (43): Xlib's sync/roundtrip. Reply: revert-to + focus window.
        43 => {
            let mut r = reply_header(c, 0);
            // The window SetInputFocus last named, not a blanket "the root has it" —
            // chrome asks this to decide whether it is the active browser window.
            let f = FOCUS_WINDOW.load(core::sync::atomic::Ordering::Relaxed);
            wr32(c, &mut r, 8, if f != 0 { f } else { ROOT_WINDOW });
            pad_reply(&mut r);
            c.outbuf.extend_from_slice(&r);
        }
        // InternAtom (16): only-if-exists in the detail byte, name length@4, name@8.
        16 => {
            let n = ru16(c, req, 4) as usize;
            let name = alloc::string::String::from_utf8_lossy(req.get(8..8 + n).unwrap_or(&[])).into_owned();
            let atom = intern_atom(&name, detail != 0);
            let mut r = reply_header(c, 0);
            wr32(c, &mut r, 8, atom);
            pad_reply(&mut r);
            c.outbuf.extend_from_slice(&r);
        }
        // GetAtomName (17): atom@4. Reply: name length@8, the name padded to 4.
        17 => {
            let atom = ru32(c, req, 4);
            match atom_name(atom) {
                Some(name) => {
                    let mut r = reply_header(c, name.len().div_ceil(4) as u32);
                    put16(c, &mut r, 8, name.len() as u16);
                    r[32..32 + name.len()].copy_from_slice(name.as_bytes());
                    c.outbuf.extend_from_slice(&r);
                }
                None => send_error(c, 5, atom, 17, 0), // BadAtom
            }
        }
        // SetSelectionOwner (22): owner@4 (None = give it up), selection@8, time@12.
        // The previous owner, if it is another window, is told with SelectionClear.
        22 => {
            let owner = ru32(c, req, 4);
            let sel = ru32(c, req, 8);
            let me = conn_index(c);
            let prev = {
                let mut t = SEL_OWNERS.lock();
                let prev = t.iter().find(|e| e.0 == sel).copied();
                t.retain(|e| e.0 != sel);
                if owner != 0 {
                    t.push((sel, owner, me));
                }
                prev
            };
            if let Some((_, pwin, pconn)) = prev {
                if pwin != owner {
                    let mut e = [0u8; 32];
                    e[0] = 29; // SelectionClear
                    e[4..8].copy_from_slice(&((crate::interrupts::ticks() * 10) as u32).to_le_bytes());
                    e[8..12].copy_from_slice(&pwin.to_le_bytes());
                    e[12..16].copy_from_slice(&sel.to_le_bytes());
                    if pconn == me {
                        e[2..4].copy_from_slice(&c.seq.to_le_bytes());
                        c.outbuf.extend_from_slice(&e);
                    } else if let Some(Some(o)) = XCONNS.lock().get_mut(pconn).map(|x| x.as_mut()) {
                        e[2..4].copy_from_slice(&o.seq.to_le_bytes());
                        o.outbuf.extend_from_slice(&e);
                    }
                }
            }
            // XFIXES: everyone who asked to hear about this selection. Chrome counts
            // these events: its clipboard "sequence number" only moves when one
            // arrives, and a paste within five seconds of the previous one is
            // answered from a cache keyed by that number. Without the event the
            // second of two quick copy-and-pastes pasted the FIRST text.
            let subs: Vec<(usize, u32)> = XFIXES_SELECT.lock().iter()
                .filter(|e| e.2 == sel && e.3 & 0x1 != 0).map(|e| (e.0, e.1)).collect();
            let now = (crate::interrupts::ticks() * 10) as u32;
            for (ci, win) in subs {
                let mut e = [0u8; 32];
                e[0] = XFIXES_EVENT; // XFixesSelectionNotify
                e[1] = 0; // subtype: SetSelectionOwner
                e[4..8].copy_from_slice(&win.to_le_bytes());
                e[8..12].copy_from_slice(&owner.to_le_bytes());
                e[12..16].copy_from_slice(&sel.to_le_bytes());
                e[16..20].copy_from_slice(&now.to_le_bytes());
                e[20..24].copy_from_slice(&now.to_le_bytes());
                if ci == me {
                    e[2..4].copy_from_slice(&c.seq.to_le_bytes());
                    c.outbuf.extend_from_slice(&e);
                } else if let Some(Some(o)) = XCONNS.lock().get_mut(ci).map(|x| x.as_mut()) {
                    e[2..4].copy_from_slice(&o.seq.to_le_bytes());
                    o.outbuf.extend_from_slice(&e);
                }
            }
            trace(format_args!("SetSelectionOwner selection={sel} owner={owner:#x}"));
        }
        // ConvertSelection (24): requestor@4, selection@8, target@12, property@16,
        // time@20. Nobody owns it, or the owner is the asking connection's own
        // window (a client answers itself without the server): SelectionNotify
        // with property None, so the requestor does not wait. Handing a request to
        // an owner on ANOTHER connection (SelectionRequest, properties, SendEvent)
        // is not implemented: that paste comes up empty.
        24 => {
            let mut e = [0u8; 32];
            e[0] = 31; // SelectionNotify
            e[2..4].copy_from_slice(&c.seq.to_le_bytes());
            e[4..8].copy_from_slice(&req[20..24]);   // time
            e[8..12].copy_from_slice(&req[4..8]);    // requestor
            e[12..16].copy_from_slice(&req[8..12]);  // selection
            e[16..20].copy_from_slice(&req[12..16]); // target
            // property@20 = None
            c.outbuf.extend_from_slice(&e);
        }
        // GetProperty (20): reply "no property" (type=None, all lengths 0).
        20 => {
            let mut r = reply_header(c, 0);
            pad_reply(&mut r);
            c.outbuf.extend_from_slice(&r);
        }
        // CreateWindow(1): wid@4, x@12(i16), y@14, w@16(u16), h@18. Allocate a window
        // with an XRGB8888 backing buffer (init dark). No reply.
        1 => {
            let id = ru32(c, req, 4);
            let x = ru16(c, req, 12) as i16;
            let y = ru16(c, req, 14) as i16;
            // GTK creates its toplevel at 0x7fff (size-unset) before it applies the
            // real size. A literal 32767 dimension would be a ~60 MiB buffer AND, if
            // reported back via ConfigureNotify, would wedge GTK's layout at that size.
            // Map an absurd (>=8192) request to a sane default so GTK's own resize
            // (set_default_size -> ConfigureWindow) then drives the final geometry.
            let raw_w = ru16(c, req, 16);
            let raw_h = ru16(c, req, 18);
            let w = if raw_w >= 8192 { 400 } else { raw_w.clamp(1, screen_w()) };
            let h = if raw_h >= 8192 { 260 } else { raw_h.clamp(1, screen_h()) };
            // CWBackPixel (0x2): the window starts filled with its background, as a
            // core-X client expects (hello-x11 asked for WhitePixel and got dark).
            let bg = value_at(c, req, 28, 32, 0x2).map(|v| v | 0xff00_0000).unwrap_or(0xff20_2020);
            let buf = alloc::vec![bg; w as usize * h as usize];
            // CreateWindow can carry an event-mask too (value-mask @28, CWEventMask=0x800).
            let em = win_event_mask(c, req, 28, 32);
            c.windows.push(XWindow { id, x, y, w, h, mapped: false, event_mask: em, buf, bg });
            if em != 0 {
                select_events(id, conn_index(c), em);
            }
            trace(format_args!("CreateWindow id={id:#x} {w}x{h} @({x},{y}) mask={em:#x} conn={}", conn_index(c)));
        }
        // CreateGC(55): cid@4, drawable@8, value-mask@12, values@16.
        55 => {
            let id = ru32(c, req, 4);
            let fg = gc_foreground(c, req, 12, 16).unwrap_or(0);
            let bg = value_at(c, req, 12, 16, 0x8).unwrap_or(0xffff_ffff);
            c.gcs.push(XGc { id, fg, bg });
            trace(format_args!("CreateGC id={id:#x} fg={fg:#08x}"));
        }
        // ChangeGC(56): gc@4, value-mask@8, values@12 (XSetForeground uses this).
        56 => {
            let id = ru32(c, req, 4);
            if let Some(fg) = gc_foreground(c, req, 8, 12) {
                if let Some(g) = c.gcs.iter_mut().find(|g| g.id == id) {
                    g.fg = fg;
                }
                trace(format_args!("ChangeGC id={id:#x} fg={fg:#08x}"));
            }
            if let Some(bg) = value_at(c, req, 8, 12, 0x8) {
                if let Some(g) = c.gcs.iter_mut().find(|g| g.id == id) {
                    g.bg = bg;
                }
            }
        }
        // ChangeWindowAttributes(2): window@4, value-mask@8, values@12. XSelectInput
        // sends this with CWEventMask (0x800) + the event mask -> store it.
        2 => {
            let id = ru32(c, req, 4);
            let em = win_event_mask(c, req, 8, 12);
            if let Some(win) = c.windows.iter_mut().find(|w| w.id == id) {
                win.event_mask |= em;
            }
            // PER-CLIENT selection — and on FOREIGN windows too. A client may select
            // events on any window it knows the id of; chrome's event connection
            // selects on the window another of its connections created, and dropping
            // that here was exactly why clicks reached a connection nobody reads.
            if em != 0 {
                select_events(id, conn_index(c), em);
            }
            trace(format_args!("ChangeWindowAttributes id={id:#x} event-mask={em:#x} conn={}", conn_index(c)));
        }
        // MapWindow(8): window@4 -> visible; present it; then deliver the events the
        // window asked for (Expose always; test key/button when injection is enabled).
        8 => {
            let id = ru32(c, req, 4);
            let (mask, x, y, w, h) = match c.windows.iter_mut().find(|w| w.id == id) {
                Some(win) => { win.mapped = true; (win.event_mask, win.x, win.y, win.w, win.h) }
                None => (0, 0, 0, 0, 0),
            };
            trace(format_args!("MapWindow id={id:#x} mask={mask:#x}"));
            present(c, id);
            const EXPOSURE: u32 = 0x8000;
            const STRUCTURE_NOTIFY: u32 = 0x0002_0000;
            const KEY_PRESS: u32 = 0x0001;
            const BUTTON_PRESS: u32 = 0x0004;
            // A toolkit (GTK) that selected StructureNotify waits for MapNotify +
            // ConfigureNotify to learn its mapped size before it size-allocates its
            // widgets and draws. Send them before Expose so the draw cycle proceeds.
            if mask & STRUCTURE_NOTIFY != 0 {
                send_map_notify(c, id);
                send_configure_notify(c, id, x, y, w, h);
            }
            if mask & EXPOSURE != 0 {
                send_expose(c, id, w, h);
            }
            if INJECT_TEST_INPUT.load(core::sync::atomic::Ordering::Relaxed) {
                if mask & KEY_PRESS != 0 {
                    send_input(c, 2, 38, id, 100, 60, 100, 60); // KeyPress, keycode 38 ('a')
                }
                if mask & BUTTON_PRESS != 0 {
                    send_input(c, 4, 1, id, 100, 60, 100, 60); // ButtonPress, button 1
                }
            }
        }
        // PolyFillRectangle(70): drawable@4, gc@8, rectangles@12 (each x,y i16; w,h u16).
        70 => {
            let draw = ru32(c, req, 4);
            let gcid = ru32(c, req, 8);
            let fg = c.gcs.iter().find(|g| g.id == gcid).map(|g| g.fg | 0xff00_0000).unwrap_or(0xffff_ffff);
            let mut off = 12;
            while off + 8 <= req.len() {
                let rx = ru16(c, req, off) as i16 as i32;
                let ry = ru16(c, req, off + 2) as i16 as i32;
                let rw = ru16(c, req, off + 4) as i32;
                let rh = ru16(c, req, off + 6) as i32;
                fill_rect(c, draw, rx, ry, rw, rh, fg);
                off += 8;
            }
            trace(format_args!("PolyFillRectangle draw={draw:#x} gc={gcid:#x} fg={fg:#08x}"));
            present(c, draw);
        }
        // PutImage(72): upload a raster into a drawable. format(detail): 2=ZPixmap.
        // drawable@4, gc@8, width@12(u16), height@14, dst-x@16(i16), dst-y@18,
        // left-pad@20, depth@21, image data@24. For ZPixmap depth>=24 (32 bpp) each
        // pixel is 4 bytes, LSBFirst (0x00RRGGBB). This is how toolkits/fonts draw.
        72 => {
            let draw = ru32(c, req, 4);
            let width = ru16(c, req, 12) as usize;
            let height = ru16(c, req, 14) as usize;
            let dst_x = ru16(c, req, 16) as i16 as i32;
            let dst_y = ru16(c, req, 18) as i16 as i32;
            let data = &req[24.min(req.len())..];
            if detail == 2 && width > 0 && height > 0 {
                put_image(c, draw, dst_x, dst_y, width, height, data);
            }
            trace(format_args!("PutImage draw={draw:#x} fmt={detail} {width}x{height} @({dst_x},{dst_y})"));
            // Verify: sample the four quadrant centres in the rendered window.
            if let Some(win) = c.windows.iter().find(|w| w.id == draw) {
                let (ww, wh) = (win.w as usize, win.h as usize);
                let at = |x: usize, y: usize| win.buf.get(y * ww + x).copied().unwrap_or(0);
                trace(format_args!(
                    "PutImage samples q(TL={:#08x} BR={:#08x}) p(100,110)={:#08x} p(220,110)={:#08x} p(150,150)={:#08x}",
                    at(ww / 4, wh / 4), at(ww * 3 / 4, wh * 3 / 4),
                    at(100.min(ww - 1), 110.min(wh - 1)), at(220.min(ww - 1), 110.min(wh - 1)), at(150.min(ww - 1), 150.min(wh - 1))
                ));
            }
            present(c, draw);
        }
        // ConfigureWindow(12): window@4, value-mask@8(u16), values@12 (4 bytes each in
        // bit order: x,y,width,height,border,sibling,stack). GTK resizes its toplevel to
        // the real size after creating it at 0x7fff; honour width/height + re-ConfigureNotify
        // so GTK size-allocates to the correct geometry (not the clamped 800).
        12 => {
            let id = ru32(c, req, 4);
            let mask = ru16(c, req, 8) as u32;
            let mut voff = 12usize;
            let (mut nw, mut nh) = (None, None);
            let (mut nx, mut ny): (Option<i16>, Option<i16>) = (None, None);
            for bit in 0..7 {
                if mask & (1 << bit) != 0 {
                    let v = ru32(c, req, voff);
                    // Ignore an absurd (unconstrained, >=8192) width/height — WM-less GTK
                    // asks for its full unconstrained size; honouring it would make a
                    // giant window. Keep the current size for those.
                    match bit {
                        // x and y: where a popup sits (a tooltip follows the pointer).
                        0 => nx = Some(v as i16),
                        1 => ny = Some(v as i16),
                        2 if v < 8192 => nw = Some((v as u16).clamp(1, screen_w())),
                        3 if v < 8192 => nh = Some((v as u16).clamp(1, screen_h())),
                        _ => {}
                    }
                    voff += 4;
                }
            }
            let mut notify = None;
            let mut moved = false;
            if let Some(win) = c.windows.iter_mut().find(|w| w.id == id) {
                if X_WINDOWED.load(core::sync::atomic::Ordering::Relaxed) {
                    let (x, y) = (nx.unwrap_or(win.x), ny.unwrap_or(win.y));
                    moved = (x, y) != (win.x, win.y) && win.mapped;
                    win.x = x;
                    win.y = y;
                }
                let w = nw.unwrap_or(win.w);
                let h = nh.unwrap_or(win.h);
                if w != win.w || h != win.h {
                    win.w = w;
                    win.h = h;
                    win.buf = alloc::vec![0xff20_2020u32; w as usize * h as usize];
                }
                if win.event_mask & 0x0002_0000 != 0 {
                    notify = Some((win.x, win.y, win.w, win.h));
                }
            }
            trace(format_args!("ConfigureWindow id={id:#x} -> {:?}x{:?} at {:?},{:?}", nw, nh, nx, ny));
            if moved {
                compose_windowed(c);
            }
            if let Some((x, y, w, h)) = notify {
                send_configure_notify(c, id, x, y, w, h);
                send_expose(c, id, w, h);
            }
        }
        // GetImage(73): read pixels back from a drawable. cairo's xlib image-fallback
        // (used when RENDER is absent — which we report) reads the destination, composites
        // glyphs/images in client memory, and writes back via PutImage. Without this the
        // window shows only solid fills and no text. Reply carries width*height ZPixmaps.
        73 => {
            let draw = ru32(c, req, 4);
            let ix = ru16(c, req, 8) as i16 as i32;
            let iy = ru16(c, req, 10) as i16 as i32;
            let iw = ru16(c, req, 12) as usize;
            let ih = ru16(c, req, 14) as usize;
            // Snapshot the requested region (window or pixmap) into a temp buffer.
            let dims = c.windows.iter().find(|w| w.id == draw).map(|w| (w.w as i32, w.h as i32))
                .or_else(|| c.pixmaps.iter().find(|p| p.id == draw).map(|p| (p.w as i32, p.h as i32)));
            let (sw, sh) = dims.unwrap_or((0, 0));
            let mut pixels = alloc::vec![0u32; iw * ih];
            if sw > 0 {
                let src: &[u32] = if let Some(w) = c.windows.iter().find(|w| w.id == draw) { &w.buf }
                    else if let Some(p) = c.pixmaps.iter().find(|p| p.id == draw) { &p.buf } else { &[] };
                for row in 0..ih {
                    let syy = iy + row as i32;
                    if syy < 0 || syy >= sh { continue; }
                    for col in 0..iw {
                        let sxx = ix + col as i32;
                        if sxx < 0 || sxx >= sw { continue; }
                        pixels[row * iw + col] = src[(syy * sw + sxx) as usize];
                    }
                }
            }
            let units = iw * ih; // 1 unit (4 bytes) per pixel
            let mut r = reply_header(c, units as u32);
            r[1] = 24; // depth
            wr32(c, &mut r, 8, ROOT_VISUAL);
            for (i, px) in pixels.iter().enumerate() {
                let o = 32 + i * 4;
                r[o..o + 4].copy_from_slice(&(px & 0x00ff_ffff).to_le_bytes()); // ZPixmap 0x00RRGGBB, LSBFirst
            }
            c.outbuf.extend_from_slice(&r);
            trace(format_args!("GetImage draw={draw:#x} {iw}x{ih} @({ix},{iy})"));
        }
        // CreatePixmap(53): depth@detail, pid@4, drawable@8, width@12, height@14.
        // An off-screen buffer GTK renders its widget tree into. No reply.
        53 => {
            let id = ru32(c, req, 4);
            let w = ru16(c, req, 12).clamp(1, screen_w());
            let h = ru16(c, req, 14).clamp(1, screen_h());
            c.pixmaps.push(XPixmap { id, w, h, buf: alloc::vec![0xff00_0000u32; w as usize * h as usize] });
            trace(format_args!("CreatePixmap id={id:#x} {w}x{h}"));
        }
        // FreePixmap(54): pid@4.
        54 => {
            let id = ru32(c, req, 4);
            c.pixmaps.retain(|p| p.id != id);
        }
        // CopyArea(62): src@4, dst@8, gc@12, src-x@16, src-y@18, dst-x@20, dst-y@22,
        // width@24, height@26. Copies a pixmap region onto the window (or vice versa)
        // — how GTK flushes its rendered widgets to the visible window. Present after.
        62 => {
            let src = ru32(c, req, 4);
            let dst = ru32(c, req, 8);
            let sx = ru16(c, req, 16) as i16 as i32;
            let sy = ru16(c, req, 18) as i16 as i32;
            let dx = ru16(c, req, 20) as i16 as i32;
            let dy = ru16(c, req, 22) as i16 as i32;
            let w = ru16(c, req, 24) as i32;
            let h = ru16(c, req, 26) as i32;
            copy_area(c, src, dst, sx, sy, dx, dy, w, h);
            trace(format_args!("CopyArea src={src:#x} dst={dst:#x} {w}x{h} @({dx},{dy})"));
            present(c, dst);
        }
        // GetSelectionOwner(23): selection@4 -> the window SetSelectionOwner named,
        // None (0) when nobody owns it.
        23 => {
            let sel = ru32(c, req, 4);
            let owner = SEL_OWNERS.lock().iter().find(|e| e.0 == sel).map(|e| e.1).unwrap_or(0);
            let mut r = reply_header(c, 0);
            wr32(c, &mut r, 8, owner);
            c.outbuf.extend_from_slice(&r);
        }
        // GetGeometry(14): drawable@4. Reply: depth in r[1], root, x,y,w,h,border.
        14 => {
            let id = ru32(c, req, 4);
            let (x, y, w, h) = c.windows.iter().find(|w| w.id == id)
                .map(|win| (win.x, win.y, win.w, win.h))
                .unwrap_or((0, 0, screen_w(), screen_h()));
            let mut r = reply_header(c, 0);
            r[1] = 24; // depth
            wr32(c, &mut r, 8, ROOT_WINDOW); // root
            put16(c, &mut r, 12, x as u16);
            put16(c, &mut r, 14, y as u16);
            put16(c, &mut r, 16, w);
            put16(c, &mut r, 18, h);
            put16(c, &mut r, 20, 0); // border width
            c.outbuf.extend_from_slice(&r);
        }
        // QueryTree(15): root, parent=None, no children.
        15 => {
            let mut r = reply_header(c, 0);
            wr32(c, &mut r, 8, ROOT_WINDOW);  // root
            wr32(c, &mut r, 12, 0);           // parent = None
            put16(c, &mut r, 16, 0);          // number of children
            c.outbuf.extend_from_slice(&r);
        }
        // GetWindowAttributes(3): a plausible InputOutput/Viewable window on our one
        // TrueColor visual. Reply is 3 extra 4-byte units (44 bytes total).
        3 => {
            let id = ru32(c, req, 4);
            let mapped = c.windows.iter().find(|w| w.id == id).map(|w| w.mapped).unwrap_or(true);
            let mut r = reply_header(c, 3);
            r[1] = 0; // backing-store = NotUseful
            wr32(c, &mut r, 8, ROOT_VISUAL);          // visual
            put16(c, &mut r, 12, 1);                  // class = InputOutput
            r[14] = 0;                                // bit gravity = Forget
            r[15] = 1;                                // win gravity = NorthWest
            wr32(c, &mut r, 16, 0);                   // backing planes
            wr32(c, &mut r, 20, 0);                   // backing pixel
            r[24] = 0;                                // save-under
            r[25] = 1;                                // map-is-installed
            r[26] = if mapped { 2 } else { 0 };       // map-state = Viewable/Unmapped
            r[27] = 0;                                // override-redirect
            wr32(c, &mut r, 28, DEFAULT_CMAP);        // colormap
            wr32(c, &mut r, 32, 0);                   // all-event-masks
            wr32(c, &mut r, 36, 0);                   // your-event-mask
            put16(c, &mut r, 40, 0);                  // do-not-propagate-mask
            c.outbuf.extend_from_slice(&r);
        }
        // QueryPointer(38): window@4. Where the pointer is, in the SAME coordinates
        // the events carry. In the desktop frame that is not the screen position
        // of the mouse: the client's root coordinates are its window's X position
        // plus the offset inside the frame (LAST_ROOT, from the last event sent).
        // Answering with the raw screen position told the client its pointer was
        // somewhere else than its own events said, by the frame's offset on the
        // desktop; after the user moved the frame, often outside the window.
        38 => {
            let (mx, my) = crate::mouse::pos();
            let (rx, ry) = if X_WINDOWED.load(core::sync::atomic::Ordering::Relaxed) {
                let p = LAST_ROOT.load(core::sync::atomic::Ordering::Relaxed);
                ((p >> 16) as i16, p as i16)
            } else {
                (mx as i16, my as i16)
            };
            let win = ru32(c, req, 4);
            let (wx, wy) = c.windows.iter().find(|w| w.id == win).map(|w| (w.x, w.y)).unwrap_or((0, 0));
            let mut r = reply_header(c, 0);
            r[1] = 1; // same-screen = true
            wr32(c, &mut r, 8, ROOT_WINDOW);   // root
            wr32(c, &mut r, 12, 0);            // child = None
            put16(c, &mut r, 16, rx as u16);   // root-x
            put16(c, &mut r, 18, ry as u16);   // root-y
            put16(c, &mut r, 20, (rx - wx) as u16); // win-x
            put16(c, &mut r, 22, (ry - wy) as u16); // win-y
            put16(c, &mut r, 24, mod_state()); // mask (buttons/mods)
            c.outbuf.extend_from_slice(&r);
        }
        // TranslateCoordinates(40): src-x@12(i16), src-y@14 → echo (windows share the
        // root's coordinate space here). child = None.
        40 => {
            let sx = ru16(c, req, 12);
            let sy = ru16(c, req, 14);
            let mut r = reply_header(c, 0);
            r[1] = 1; // same-screen
            wr32(c, &mut r, 8, 0); // child = None
            put16(c, &mut r, 12, sx);
            put16(c, &mut r, 14, sy);
            c.outbuf.extend_from_slice(&r);
        }
        // GetMotionEvents(39): no history.
        39 => {
            let mut r = reply_header(c, 0);
            wr32(c, &mut r, 8, 0); // number of events = 0
            c.outbuf.extend_from_slice(&r);
        }
        // GetKeyboardMapping(101): first-keycode@4, count@5. Four keysyms per
        // keycode from the active layout: plain, Shift, AltGr, AltGr+Shift (the
        // second pair is group 2, which Mode_switch selects). Keycodes are the
        // evdev code + 8, so a toolkit maps keycode -> keysym -> character.
        101 => {
            let first = req.get(4).copied().unwrap_or(8);
            let count = req.get(5).copied().unwrap_or(0) as usize;
            const N: usize = 4; // keysyms per keycode
            let mut r = reply_header(c, (count * N) as u32);
            r[1] = N as u8;
            for i in 0..count {
                let kc = first.wrapping_add(i as u8);
                let ks = keysyms4(kc);
                for (k, sym) in ks.iter().enumerate() {
                    let base = 32 + (i * N + k) * 4;
                    put32(c, &mut r, base, *sym);
                }
            }
            c.outbuf.extend_from_slice(&r);
            trace(format_args!("GetKeyboardMapping first={first} count={count}"));
        }
        // GetModifierMapping(119): keycodes-per-modifier in r[1]; 8 modifiers ×
        // that many keycodes (Shift, Lock, Control, Mod1..5). Provide the standard set.
        119 => {
            const KPM: usize = 2; // keycodes per modifier
            // Data = 8 modifiers × KPM keycodes × 1 byte = 8*KPM bytes = 2*KPM units.
            let mut r = reply_header(c, (2 * KPM) as u32);
            r[1] = KPM as u8;
            // Modifier index: 0 Shift, 1 Lock, 2 Control, 3 Mod1 (Alt), 4 Mod2
            // (Num_Lock), 7 Mod5 (Mode_switch = AltGr, the second keysym group).
            let mods: [(usize, [u8; KPM]); 6] = [
                (0, [50, 62]), (1, [66, 0]), (2, [37, 105]), (3, [64, 0]), (4, [77, 0]), (7, [108, 0]),
            ];
            for (m, kcs) in mods {
                r[32 + m * KPM..32 + m * KPM + KPM].copy_from_slice(&kcs);
            }
            c.outbuf.extend_from_slice(&r);
            trace(format_args!("GetModifierMapping"));
        }
        // GetKeyboardControl(103): a plausible reply (global auto-repeat on, etc.).
        103 => {
            let mut r = reply_header(c, 5); // 5 extra units = 20 bytes (bell/led/repeat map)
            r[1] = 1; // global-auto-repeat = On
            c.outbuf.extend_from_slice(&r);
        }
        // QueryKeymap(44): 32-byte bitmap of currently-pressed keys (all zero = none).
        // Reply is 8-byte header + 32 keys = length 2 units beyond the 32-byte base.
        44 => {
            let r = reply_header(c, 2); // 40 bytes, keys @8..40 already zero
            c.outbuf.extend_from_slice(&r);
        }
        // QueryFont(47): the metrics of the one server font (the 8x8 bitmap font
        // every core-text request draws with): fixed 8-pixel advance, ascent 7,
        // descent 1, ASCII 32..126, no per-char table (Xlib then uses min_bounds
        // for every glyph, so XTextWidth = 8 * len). Reply = 32 + 7 units.
        47 => {
            let mut r = reply_header(c, 7);
            for base in [8usize, 24] { // min_bounds @8, max_bounds @24 (12 bytes each)
                put16(c, &mut r, base, 0);      // left-side-bearing
                put16(c, &mut r, base + 2, 8);  // right-side-bearing
                put16(c, &mut r, base + 4, 8);  // character-width
                put16(c, &mut r, base + 6, 7);  // ascent
                put16(c, &mut r, base + 8, 1);  // descent
            }
            put16(c, &mut r, 40, 32);  // min-char-or-byte2
            put16(c, &mut r, 42, 126); // max-char-or-byte2
            put16(c, &mut r, 44, 32);  // default-char
            put16(c, &mut r, 46, 0);   // n-properties
            r[48] = 0; r[49] = 0; r[50] = 0; r[51] = 1; // draw-direction, min/max byte1, all-chars-exist
            put16(c, &mut r, 52, 7);   // font-ascent
            put16(c, &mut r, 54, 1);   // font-descent
            put32(c, &mut r, 56, 0);   // n-char-infos
            c.outbuf.extend_from_slice(&r);
        }
        // ClearArea(61): window@4, x@8, y@10 (i16), w@12, h@14 (u16, 0 = to the edge):
        // paint the window background back; exposures(detail) sends Expose for it.
        61 => {
            let id = ru32(c, req, 4);
            let rx = ru16(c, req, 8) as i16 as i32;
            let ry = ru16(c, req, 10) as i16 as i32;
            let mut rw = ru16(c, req, 12) as i32;
            let mut rh = ru16(c, req, 14) as i32;
            if let Some(win) = c.windows.iter_mut().find(|w| w.id == id) {
                if rw == 0 { rw = win.w as i32 - rx; }
                if rh == 0 { rh = win.h as i32 - ry; }
                let bg = win.bg;
                fill_buf(&mut win.buf, win.w as i32, win.h as i32, rx, ry, rw, rh, bg);
            }
            present(c, id);
            if detail != 0 {
                send_expose(c, id, rw.max(0) as u16, rh.max(0) as u16);
            }
        }
        // PolyText8(74): drawable@4, gc@8, x@12, y@14 (i16, baseline), then TEXTITEM8s:
        // (len u8, delta i8, string[len]); len 255 = a font shift (5 bytes, ignored).
        // XDrawString ends here. Glyphs come from the kernel's 8x8 bitmap font.
        74 => {
            let draw = ru32(c, req, 4);
            let gcid = ru32(c, req, 8);
            let mut x = ru16(c, req, 12) as i16 as i32;
            let y = ru16(c, req, 14) as i16 as i32;
            let fg = c.gcs.iter().find(|g| g.id == gcid).map(|g| g.fg | 0xff00_0000).unwrap_or(0xff00_0000);
            let mut off = 16;
            while off + 2 <= req.len() {
                let len = req[off] as usize;
                if len == 255 { off += 5; continue; }
                if len == 0 { break; }
                let delta = req[off + 1] as i8 as i32;
                let end = (off + 2 + len).min(req.len());
                let text: Vec<u8> = req[off + 2..end].to_vec();
                x += delta;
                draw_text8(c, draw, x, y, &text, fg, None);
                x += 8 * text.len() as i32;
                off = end;
            }
            present(c, draw);
        }
        // ImageText8(76): drawable@4, gc@8, x@12, y@14, string@16 (len = detail);
        // the glyph cells are filled with the GC background first (XDrawImageString).
        76 => {
            let draw = ru32(c, req, 4);
            let gcid = ru32(c, req, 8);
            let x = ru16(c, req, 12) as i16 as i32;
            let y = ru16(c, req, 14) as i16 as i32;
            let (fg, bg) = c.gcs.iter().find(|g| g.id == gcid)
                .map(|g| (g.fg | 0xff00_0000, g.bg | 0xff00_0000)).unwrap_or((0xff00_0000, 0xffff_ffff));
            let n = (detail as usize).min(req.len().saturating_sub(16));
            let text: Vec<u8> = req[16..16 + n].to_vec();
            draw_text8(c, draw, x, y, &text, fg, Some(bg));
            present(c, draw);
        }
        // Bell(104)/ChangeKeyboardControl(102)/NoOperation(127): no reply.
        104 | 102 | 127 => {}
        // Everything else (no reply): acknowledged by consuming the request.
        // UnmapWindow (10) / DestroyWindow (4): a dialog that is dismissed must stop
        // being on screen AND stop being a click target. Neither was handled at all,
        // so a closed dialog stayed forever in front of the browser as far as input
        // routing was concerned, and swallowed every click aimed at the page behind it.
        4 | 10 => {
            let id = ru32(c, req, 4);
            // Its pixels have to go too. The fullscreen blit only ever paints, so a
            // dismissed dialog would otherwise stay on screen after it stopped
            // existing — and the screen is what a person believes.
            {
                let mut t = PRESENTED.lock();
                if let Some(r) = t.iter().find(|r| r.0 == id) {
                    if !X_WINDOWED.load(core::sync::atomic::Ordering::Relaxed) {
                        crate::screen_clear_rect(r.1.max(0) as usize, r.2.max(0) as usize,
                                                 (r.4 * r.3).max(0) as usize, (r.5 * r.3).max(0) as usize);
                    }
                }
                t.retain(|r| r.0 != id);
            }
            if FOCUS_WINDOW.load(core::sync::atomic::Ordering::Relaxed) == id {
                FOCUS_WINDOW.store(0, core::sync::atomic::Ordering::Relaxed);
            }
            if POINTER_WIN.load(core::sync::atomic::Ordering::Relaxed) == id {
                POINTER_WIN.store(0, core::sync::atomic::Ordering::Relaxed);
            }
            if opcode == 4 {
                SEL_OWNERS.lock().retain(|e| e.1 != id);
                c.windows.retain(|w| w.id != id);
                SELECTIONS.lock().retain(|(w, _, _)| *w != id);
            } else if let Some(win) = c.windows.iter_mut().find(|w| w.id == id) {
                win.mapped = false;
            }
            trace(format_args!("{} id={id:#x}", if opcode == 4 { "DestroyWindow" } else { "UnmapWindow" }));
            if IMPLICIT_GRAB.load(core::sync::atomic::Ordering::Relaxed) == id {
                IMPLICIT_GRAB.store(0, core::sync::atomic::Ordering::Relaxed);
            }
            if X_WINDOWED.load(core::sync::atomic::Ordering::Relaxed) {
                compose_windowed(c); // the popup is gone from the frame
            }
        }
        // GrabPointer (26) / GrabKeyboard (31): chrome grabs both — for menus, for
        // drags, for its own modal dialogs. Both expect a REPLY, and the old default
        // arm answered nothing at all, which parks the browser on a reply that never
        // comes. There is one client and no window manager here, so nothing has to be
        // arbitrated: the grab is granted, status = 0 (Success).
        26 | 31 => {
            let mut r = reply_header(c, 0);
            r[1] = 0; // status: Success
            pad_reply(&mut r);
            c.outbuf.extend_from_slice(&r);
            trace(format_args!("grab op={opcode} -> Success"));
        }
        // UngrabPointer(27) GrabButton(28) UngrabButton(29) ChangeActivePointerGrab(30)
        // UngrabKeyboard(32) GrabKey(33) UngrabKey(34) AllowEvents(35): no reply is
        // owed, and with a single client there is nothing to arbitrate. Acknowledged by
        // doing nothing — but listed explicitly, so the silence is a decision.
        27 | 28 | 29 | 30 | 32 | 33 | 34 | 35 => {}
        // SetInputFocus (42): remember who holds the keyboard, and TELL that window.
        // A toolkit only starts handling keys (and blinking a caret) once it has seen
        // a FocusIn of its own; without one, keys arrive and are dropped.
        42 => {
            let win = ru32(c, req, 4);
            if win > 1 {
                // 0 = None, 1 = PointerRoot: neither names a real window.
                FOCUS_WINDOW.store(win, core::sync::atomic::Ordering::Relaxed);
                let mut e = [0u8; 32];
                e[0] = 9; // FocusIn
                e[1] = 0; // detail = NotifyAncestor
                e[2..4].copy_from_slice(&c.seq.to_le_bytes());
                e[4..8].copy_from_slice(&win.to_le_bytes());
                c.outbuf.extend_from_slice(&e);
                trace(format_args!("SetInputFocus -> win {win:#x} (+FocusIn)"));
            }
        }
        // WarpPointer (41): the client moves the cursor itself. dst-window None(0)
        // means "relative to where the pointer is now"; a real window means the
        // coordinates are relative to that window's origin.
        41 => {
            let dst = ru32(c, req, 8);
            let dx = ru16(c, req, 20) as i16 as i32;
            let dy = ru16(c, req, 22) as i16 as i32;
            let (px, py) = crate::mouse::pos();
            let (nx, ny) = if dst == 0 {
                (px as i32 + dx, py as i32 + dy)
            } else {
                match c.windows.iter().find(|w| w.id == dst) {
                    Some(w) => (w.x as i32 + dx, w.y as i32 + dy),
                    None => (dx, dy),
                }
            };
            crate::mouse::set_pos(nx.max(0) as usize, ny.max(0) as usize);
            trace(format_args!("WarpPointer -> ({nx},{ny})"));
        }
        // GetPointerMapping (117): the physical-to-logical button map. Seven
        // buttons, identity: 1-3 the buttons, 4-7 the wheel (up, down, left, right).
        // Reply: map length in byte 1, then the map padded to 4.
        117 => {
            const BUTTONS: usize = 7;
            let mut r = reply_header(c, BUTTONS.div_ceil(4) as u32);
            r[1] = BUTTONS as u8;
            for i in 0..BUTTONS {
                r[32 + i] = (i + 1) as u8;
            }
            c.outbuf.extend_from_slice(&r);
            trace(format_args!("GetPointerMapping -> {BUTTONS} buttons"));
        }
        // A request that owes a reply and has no handler above must still be
        // answered: a client that waits for a reply waits forever (BUG-090: xcb
        // sat in poll() on GetPointerMapping, the browser's first connection never
        // got past its setup). The answer is the X error BadImplementation, which
        // every client library turns into a failed call instead of a hang.
        _ if owes_reply(opcode) => {
            send_error(c, 17, 0, opcode, 0);
            crate::serial_println!("[xserver] request {opcode} (owes a reply) is not implemented: answered with BadImplementation");
        }
        _ => {}
    }
}

/// Core requests that the client waits on (X11 protocol, appendix B). Extension
/// requests (128 and up) never arrive: QueryExtension reports none present.
fn owes_reply(opcode: u8) -> bool {
    matches!(opcode,
        3 | 14 | 15 | 16 | 17 | 20 | 21 | 23 | 26 | 31 | 38 | 39 | 40 | 43 | 44 | 47 | 48 | 49 | 50 | 52
        | 73 | 83 | 84 | 85 | 86 | 87 | 91 | 92 | 97 | 98 | 99 | 101 | 103 | 106 | 108 | 110 | 116 | 117
        | 118 | 119)
}

/// Queue an X error for the request being handled: code, the offending value,
/// and the major/minor opcode it answers (32 bytes, byte 0 = 0).
fn send_error(c: &mut XConn, code: u8, bad_value: u32, major: u8, minor: u16) {
    let mut e = alloc::vec![0u8; 32];
    e[1] = code;
    put16(c, &mut e, 2, c.seq);
    put32(c, &mut e, 4, bad_value);
    put16(c, &mut e, 8, minor);
    e[10] = major;
    c.outbuf.extend_from_slice(&e);
}

/// Copy a ZPixmap (32-bpp, LSBFirst) into the target window buffer, clipped.
/// Blit a 32-bpp ZPixmap into a drawable buffer at (dst_x,dst_y), clipped.
fn blit_image(buf: &mut [u32], ww: i32, wh: i32, dst_x: i32, dst_y: i32, w: usize, h: usize, data: &[u8]) {
    let stride = w * 4; // 32 bpp, scanline already 4-aligned
    for sy in 0..h {
        let dy = dst_y + sy as i32;
        if dy < 0 || dy >= wh { continue; }
        for sx in 0..w {
            let dx = dst_x + sx as i32;
            if dx < 0 || dx >= ww { continue; }
            let o = sy * stride + sx * 4;
            if o + 4 > data.len() { break; }
            let px = u32::from_le_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]]);
            buf[(dy * ww + dx) as usize] = 0xff00_0000 | (px & 0x00ff_ffff);
        }
    }
}

fn put_image(c: &mut XConn, draw: u32, dst_x: i32, dst_y: i32, w: usize, h: usize, data: &[u8]) {
    if let Some(win) = c.windows.iter_mut().find(|win| win.id == draw) {
        blit_image(&mut win.buf, win.w as i32, win.h as i32, dst_x, dst_y, w, h, data);
    } else if let Some(pm) = c.pixmaps.iter_mut().find(|p| p.id == draw) {
        blit_image(&mut pm.buf, pm.w as i32, pm.h as i32, dst_x, dst_y, w, h, data);
    } else {
        // A drawable owned by ANOTHER connection: X ids are server-global, and
        // chrome paints the browser window over a different connection than the
        // one that created it. (The processing connection is outside the table,
        // so this lock cannot alias `c`.)
        let mut t = XCONNS.lock();
        for conn in t.iter_mut().flatten() {
            if let Some(win) = conn.windows.iter_mut().find(|win| win.id == draw) {
                blit_image(&mut win.buf, win.w as i32, win.h as i32, dst_x, dst_y, w, h, data);
                return;
            }
            if let Some(pm) = conn.pixmaps.iter_mut().find(|p| p.id == draw) {
                blit_image(&mut pm.buf, pm.w as i32, pm.h as i32, dst_x, dst_y, w, h, data);
                return;
            }
        }
    }
}

/// When set, MapWindow also delivers a synthetic KeyPress + ButtonPress to a window
/// that selected them — so event delivery is testable without live hardware input.
/// (Real apps get only legitimate events; this is gated for the gxevent self-test.)
pub static INJECT_TEST_INPUT: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// True while a persistent, desktop-integrated X client owns the screen. The desktop
/// loop checks this to route live keyboard/mouse into the X server (pump_*) instead
/// of the DOOM-style appgfx bridge.
pub static X_APP_ACTIVE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
pub fn x_app_active() -> bool { X_APP_ACTIVE.load(core::sync::atomic::Ordering::Relaxed) }

/// WINDOWED mode: when true, an X client's window is NOT blitted fullscreen by
/// `present()`; instead the desktop compositor pulls its pixels (with_front_window)
/// and draws it as a framed desktop window. Set by the desktop when it hosts an X app.
pub static X_WINDOWED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
pub fn set_windowed(on: bool) { X_WINDOWED.store(on, core::sync::atomic::Ordering::Relaxed); }

/// A hosted app is about to start: it gets an EMPTY server. A client that ended
/// without closing its socket (every X client of the boot self-tests; an app that
/// was closed with the close button of its frame) stays in the table with its
/// windows mapped. Since BUG-097 the frame shows every mapped window of every
/// connection, so the next app came up with the pictures of the dead clients
/// drawn over its own, and its keys went to the window of a dead client that was
/// presented last (BUG-113). One hosted app runs at a time and none runs when
/// the desktop calls this, so whatever is still connected is gone. It also
/// gives the slots back: there are MAX_XCONN of them.
pub fn forget_clients() {
    let gone = {
        let mut t = lock_home();
        let n = t.iter().flatten().count();
        for c in t.iter_mut() {
            *c = None;
        }
        n
    };
    PRESENTED.lock().clear();
    SELECTIONS.lock().clear();
    SEL_OWNERS.lock().clear();
    XFIXES_SELECT.lock().clear();
    READER_CONNS.lock().clear();
    FOCUS_WINDOW.store(0, core::sync::atomic::Ordering::Relaxed);
    RETAINED_ID.store(0, core::sync::atomic::Ordering::Relaxed);
    IMPLICIT_GRAB.store(0, core::sync::atomic::Ordering::Relaxed);
    LAST_ROOT.store(0, core::sync::atomic::Ordering::Relaxed);
    *RETAINED_WINDOW.lock() = None;
    if gone > 0 {
        crate::serial_println!("[xserver] a hosted app starts: {gone} connection(s) of clients that are gone were forgotten");
    }
}
pub fn x_windowed() -> bool { X_WINDOWED.load(core::sync::atomic::Ordering::Relaxed) }

/// The RETAINED pixel buffer of a hosted X client's window: (w, h, pixels 0x00RRGGBB).
/// Captured by `present()` in windowed mode so the desktop can composite the app as a
/// framed window — and keep showing it even after the (boot-run) client exits.
pub static RETAINED_WINDOW: Mutex<Option<(usize, usize, Vec<u32>)>> = Mutex::new(None);

/// Hand the retained hosted-X-window pixels to `f` (w, h, &buf). Returns true if there
/// is a retained window. The desktop compositor (task 0) calls this — hold the lock
/// IRQ-SAFE so a timer preemption can't leave it held while the (IF=0, non-preemptible)
/// glibc app spins on the same lock inside present() → deadlock (BUG-007 class).
pub fn with_front_window(f: impl FnOnce(usize, usize, &[u32])) -> bool {
    x86_64::instructions::interrupts::without_interrupts(|| {
        if let Some((w, h, buf)) = RETAINED_WINDOW.lock().as_ref() {
            f(*w, *h, buf);
            true
        } else {
            false
        }
    })
}

/// (w, h) of the retained hosted X window, for the desktop to size its frame.
pub fn front_window_size() -> Option<(usize, usize)> {
    x86_64::instructions::interrupts::without_interrupts(|| {
        RETAINED_WINDOW.lock().as_ref().map(|(w, h, _)| (*w, *h))
    })
}

/// Extract a window's event-mask from an attribute value-list, if CWEventMask is set.
/// The mask word is at `mask_off`; packed values start at `vals_off` in bit order.
fn win_event_mask(c: &XConn, req: &[u8], mask_off: usize, vals_off: usize) -> u32 {
    const CW_EVENT_MASK: u32 = 0x0000_0800;
    let mask = ru32(c, req, mask_off);
    if mask & CW_EVENT_MASK == 0 {
        return 0;
    }
    let idx = (mask & (CW_EVENT_MASK - 1)).count_ones() as usize;
    let at = vals_off + idx * 4;
    if at + 4 <= req.len() { ru32(c, req, at) } else { 0 }
}

/// Queue a 32-byte Expose event for a window (sent on map so the client repaints).
fn send_expose(c: &mut XConn, window: u32, w: u16, h: u16) {
    let mut e = [0u8; 32];
    e[0] = 12; // Expose
    e[2..4].copy_from_slice(&c.seq.to_le_bytes());
    e[4..8].copy_from_slice(&window.to_le_bytes());
    // x@8=0, y@10=0, width@12, height@14, count@16=0
    e[12..14].copy_from_slice(&w.to_le_bytes());
    e[14..16].copy_from_slice(&h.to_le_bytes());
    c.outbuf.extend_from_slice(&e);
    trace(format_args!("-> Expose window={window:#x} {w}x{h}"));
}

/// Queue a MapNotify(19) — tells the client its window is now mapped/viewable.
fn send_map_notify(c: &mut XConn, window: u32) {
    let mut e = [0u8; 32];
    e[0] = 19; // MapNotify
    e[2..4].copy_from_slice(&c.seq.to_le_bytes());
    e[4..8].copy_from_slice(&window.to_le_bytes());  // event window
    e[8..12].copy_from_slice(&window.to_le_bytes()); // window
    // override-redirect@12 = 0
    c.outbuf.extend_from_slice(&e);
    trace(format_args!("-> MapNotify window={window:#x}"));
}

/// Queue a ConfigureNotify(22) with the window's geometry. GTK/GDK waits for this
/// to learn its real size, size-allocate its widget tree, and only THEN draw.
fn send_configure_notify(c: &mut XConn, window: u32, x: i16, y: i16, w: u16, h: u16) {
    let mut e = [0u8; 32];
    e[0] = 22; // ConfigureNotify
    e[2..4].copy_from_slice(&c.seq.to_le_bytes());
    e[4..8].copy_from_slice(&window.to_le_bytes());  // event window
    e[8..12].copy_from_slice(&window.to_le_bytes()); // window
    // above-sibling@12 = None(0)
    e[16..18].copy_from_slice(&x.to_le_bytes());
    e[18..20].copy_from_slice(&y.to_le_bytes());
    e[20..22].copy_from_slice(&w.to_le_bytes());
    e[22..24].copy_from_slice(&h.to_le_bytes());
    // border-width@24 = 0, override-redirect@26 = 0
    c.outbuf.extend_from_slice(&e);
    trace(format_args!("-> ConfigureNotify window={window:#x} {w}x{h} @({x},{y})"));
}

/// Queue a 32-byte input event (KeyPress=2/ButtonPress=4/MotionNotify=6/Enter=7).
/// `detail` is the keycode or button number; (rx,ry) the ROOT (screen) coordinates
/// and (ex,ey) the WINDOW-LOCAL ones. The two are not interchangeable: a toolkit
/// hit-tests its widgets on the window-local pair and positions menus on the root
/// pair, so a window drawn at an offset needs both to be right.
fn send_input(c: &mut XConn, kind: u8, detail: u8, window: u32, rx: i16, ry: i16, ex: i16, ey: i16) {
    let mut e = [0u8; 32];
    e[0] = kind;
    e[1] = detail;
    e[2..4].copy_from_slice(&c.seq.to_le_bytes());
    // time@4: a real, monotonically-increasing server time (ms). GDK's key/focus
    // dispatch tracks event time (double-click, key-repeat); time=0 made it drop keys.
    let t = (crate::interrupts::ticks() * 10) as u32;
    e[4..8].copy_from_slice(&t.to_le_bytes());
    // root@8=ROOT, event(window)@12, child@16=0
    e[8..12].copy_from_slice(&ROOT_WINDOW.to_le_bytes());
    e[12..16].copy_from_slice(&window.to_le_bytes());
    // root-x@20, root-y@22 (screen), event-x@24, event-y@26 (window-local)
    e[20..22].copy_from_slice(&rx.to_le_bytes());
    e[22..24].copy_from_slice(&ry.to_le_bytes());
    e[24..26].copy_from_slice(&ex.to_le_bytes());
    e[26..28].copy_from_slice(&ey.to_le_bytes());
    // state@28: the modifier + button mask AS IT WAS BEFORE THIS EVENT (the X
    // protocol's definition). For a ButtonRelease that means the released button
    // is still set; for a ButtonPress it is not yet. The live mouse state cannot
    // supply that for a synthesized desktop click (press and release are queued
    // together, the physical button is already up), and the difference decides
    // whether the click counts: chrome derives a release's flags from this word,
    // and a button only fires when the release carries the left-button flag. The
    // delivered bytes had state=0 on both; the button painted hover and never
    // fired. Measured, not guessed: [xin] bytes=[05, 01, .., 00, 00, 01, 00].
    let button_bit: u16 = match detail { 1 => 0x100, 2 => 0x200, 3 => 0x400, 4 => 0x800, 5 => 0x1000, _ => 0 };
    let state = match kind {
        4 => mod_state() & !button_bit,
        5 => mod_state() | button_bit,
        _ => mod_state(),
    };
    e[28..30].copy_from_slice(&state.to_le_bytes());
    e[30] = 1; // same-screen
    c.outbuf.extend_from_slice(&e);
    // Unconditional (TRACE is off on the desktop path): a press or release always
    // says which connection and window it went to and how much sits queued behind
    // it; motion only the first few times. This is the instrument for "the click
    // was queued, the dialog stayed": it separates delivered-to-the-wrong-window
    // from delivered-and-never-read.
    {
        static MOTION_LEFT: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(6);
        let say = kind == 4 || kind == 5
            || (kind == 6 && MOTION_LEFT.fetch_update(core::sync::atomic::Ordering::Relaxed,
                core::sync::atomic::Ordering::Relaxed, |v| if v > 0 { Some(v - 1) } else { None }).is_ok());
        if say {
            crate::serial_println!("[xin] -> kind={kind} detail={detail} conn={} win={window:#x} local({ex},{ey}) root({rx},{ry}) queued={} B",
                conn_index(c), c.outbuf.len());
            if kind == 4 || kind == 5 {
                crate::serial_println!("[xin]    bytes={:02x?}", &e[..]);
            }
            // The window world at the first press, once, from the one function every
            // input route ends in (the desktop's hosted-frame click never passes
            // deliver_pointer or deliver_selected). `c` is this connection, already
            // borrowed; the others come from the table, locked per statement.
            if kind == 4 {
                static ONCE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
                if !ONCE.swap(true, core::sync::atomic::Ordering::Relaxed) {
                    let presented: Vec<_> = PRESENTED.lock().clone();
                    crate::serial_println!("[xin] census: retained={:#x} focus={:#x} presented(id,dx,dy,sc,w,h,ord)={presented:?}",
                        RETAINED_ID.load(core::sync::atomic::Ordering::Relaxed),
                        FOCUS_WINDOW.load(core::sync::atomic::Ordering::Relaxed));
                    let sel: Vec<_> = SELECTIONS.lock().clone();
                    crate::serial_println!("[xin]   selections(win,conn,mask)={sel:?} readers={:?}", READER_CONNS.lock().clone());
                    let wins: Vec<_> = c.windows.iter().map(|w| (w.id, w.x, w.y, w.w, w.h, w.mapped, w.event_mask)).collect();
                    crate::serial_println!("[xin]   this conn {}: windows(id,x,y,w,h,mapped,mask)={wins:?}", conn_index(c));
                    // The other connections' windows. XCONNS may be locked by our
                    // caller (deliver_to_shown holds it); try_lock, never block.
                    if let Some(t) = XCONNS.try_lock() {
                        for (ci, oc) in t.iter().enumerate() {
                            if let Some(oc) = oc {
                                let w: Vec<_> = oc.windows.iter().map(|w| (w.id, w.x, w.y, w.w, w.h, w.mapped, w.event_mask)).collect();
                                crate::serial_println!("[xin]   conn {ci}: windows={w:?}");
                            }
                        }
                    } else {
                        crate::serial_println!("[xin]   (XCONNS held by the caller; other connections not listed)");
                    }
                }
            }
        }
    }
    // The queue length answers the question a click that changes nothing raises: did
    // the client ever COLLECT the event? A number that keeps growing means the events
    // are piling up unread, and no amount of aiming at the right pixel would help.
    trace(format_args!("-> input kind={kind} detail={detail} window={window:#x} at({ex},{ey}) queued={} B",
        c.outbuf.len()));
    // Events piling up unread: the client is not collecting them. This used to
    // dump every thread, the futex table, the syscall histogram and the epoll sets
    // as soon as FOUR events were queued, at most every 30 s: the instrument for a
    // browser that never read its connection (BUG-090). With a browser that does
    // read, four queued events is a person typing, and the dump (some 400 lines
    // on the serial port with interrupts off) was itself the stall: the USB
    // keyboard's reports were not collected while it printed, and three or four
    // characters of a typed word were lost (BUG-095). Now: one line, and only
    // for a backlog no healthy client has (512 events). `cdiag` in the shell
    // prints the rest on request.
    if c.outbuf.len() >= 16 * 1024 {
        let now = crate::interrupts::ticks();
        let last = STALL_LAST_DUMP.load(core::sync::atomic::Ordering::Relaxed);
        if now >= last + 3000
            && STALL_LAST_DUMP.compare_exchange(last, now,
                core::sync::atomic::Ordering::Relaxed, core::sync::atomic::Ordering::Relaxed).is_ok()
        {
            crate::serial_println!("[xserver] {} B of input queued unread on connection {}: the client is not reading (cdiag shows its threads)",
                c.outbuf.len(), conn_index(c));
        }
    }
}

/// Extract the GCForeground value from a GC value-list, if the mask sets it. The
/// mask is at `mask_off`; the packed 4-byte values start at `vals_off` in bit order.
fn gc_foreground(c: &XConn, req: &[u8], mask_off: usize, vals_off: usize) -> Option<u32> {
    const GC_FOREGROUND: u32 = 0x0000_0004;
    let mask = ru32(c, req, mask_off);
    if mask & GC_FOREGROUND == 0 {
        return None;
    }
    let idx = (mask & (GC_FOREGROUND - 1)).count_ones() as usize; // values below foreground
    let at = vals_off + idx * 4;
    if at + 4 <= req.len() { Some(ru32(c, req, at)) } else { None }
}

/// The value for `bit` of an X value-list (CreateWindow/CreateGC/ChangeGC style:
/// a value-mask at `mask_off`, then one u32 per set bit below it, in bit order).
fn value_at(c: &XConn, req: &[u8], mask_off: usize, vals_off: usize, bit: u32) -> Option<u32> {
    let mask = ru32(c, req, mask_off);
    if mask & bit == 0 {
        return None;
    }
    let idx = (mask & (bit - 1)).count_ones() as usize;
    let at = vals_off + idx * 4;
    if at + 4 <= req.len() { Some(ru32(c, req, at)) } else { None }
}

/// Core-X text: draw `text` with the kernel's 8x8 bitmap font at baseline (x, y)
/// into a drawable (window or pixmap); `bg` fills each 8x8 cell first (ImageText).
fn draw_text8(c: &mut XConn, draw: u32, x: i32, y: i32, text: &[u8], fg: u32, bg: Option<u32>) {
    let (buf, ww, wh): (&mut Vec<u32>, i32, i32) =
        if let Some(win) = c.windows.iter_mut().find(|w| w.id == draw) { (&mut win.buf, win.w as i32, win.h as i32) }
        else if let Some(pm) = c.pixmaps.iter_mut().find(|p| p.id == draw) { (&mut pm.buf, pm.w as i32, pm.h as i32) }
        else { return };
    let top = y - 7; // ascent 7 of the 8-row glyph cell (row 7 carries descenders)
    for (i, &ch) in text.iter().enumerate() {
        let cx = x + 8 * i as i32;
        if let Some(bg) = bg {
            fill_buf(buf, ww, wh, cx, top, 8, 8, bg);
        }
        let ascii = ch as usize;
        if !(33..=126).contains(&ascii) {
            continue;
        }
        let glyph = &crate::font::FONT_DATA[ascii - 32];
        for (row, bits) in glyph.iter().enumerate() {
            let yy = top + row as i32;
            if yy < 0 || yy >= wh { continue; }
            for col in 0..8 {
                if bits & (1 << col) != 0 { // LSB = left pixel (see font::draw_char)
                    let xx = cx + col as i32;
                    if xx >= 0 && xx < ww {
                        buf[(yy * ww + xx) as usize] = fg;
                    }
                }
            }
        }
    }
}

/// Fill a rectangle in the target window's buffer (clipped) with `argb`.
fn fill_buf(buf: &mut [u32], ww: i32, wh: i32, rx: i32, ry: i32, rw: i32, rh: i32, argb: u32) {
    for yy in ry.max(0)..(ry + rh).min(wh) {
        for xx in rx.max(0)..(rx + rw).min(ww) {
            buf[(yy * ww + xx) as usize] = argb;
        }
    }
}

fn fill_rect(c: &mut XConn, draw: u32, rx: i32, ry: i32, rw: i32, rh: i32, argb: u32) {
    if let Some(win) = c.windows.iter_mut().find(|w| w.id == draw) {
        fill_buf(&mut win.buf, win.w as i32, win.h as i32, rx, ry, rw, rh, argb);
    } else if let Some(pm) = c.pixmaps.iter_mut().find(|p| p.id == draw) {
        fill_buf(&mut pm.buf, pm.w as i32, pm.h as i32, rx, ry, rw, rh, argb);
    }
}

/// CopyArea: copy a rectangle from one drawable (window or pixmap) into another.
/// GTK renders its widget tree into a pixmap and CopyAreas it onto the window.
fn copy_area(c: &mut XConn, src: u32, dst: u32, sx: i32, sy: i32, dx: i32, dy: i32, w: i32, h: i32) {
    // Snapshot the source region (pixmap or window) into a temp buffer.
    let src_dims = c.windows.iter().find(|w| w.id == src).map(|w| (w.w as i32, w.h as i32))
        .or_else(|| c.pixmaps.iter().find(|p| p.id == src).map(|p| (p.w as i32, p.h as i32)));
    let (sw, sh) = match src_dims { Some(v) => v, None => return };
    let mut tmp = alloc::vec![0u32; (w.max(0) * h.max(0)) as usize];
    {
        let src_buf: &[u32] = if let Some(win) = c.windows.iter().find(|w| w.id == src) { &win.buf }
            else if let Some(pm) = c.pixmaps.iter().find(|p| p.id == src) { &pm.buf } else { return };
        for row in 0..h {
            let syy = sy + row;
            if syy < 0 || syy >= sh { continue; }
            for col in 0..w {
                let sxx = sx + col;
                if sxx < 0 || sxx >= sw { continue; }
                tmp[(row * w + col) as usize] = src_buf[(syy * sw + sxx) as usize];
            }
        }
    }
    // Blit into the destination.
    let put = |buf: &mut [u32], dw: i32, dh: i32| {
        for row in 0..h {
            let dyy = dy + row;
            if dyy < 0 || dyy >= dh { continue; }
            for col in 0..w {
                let dxx = dx + col;
                if dxx < 0 || dxx >= dw { continue; }
                buf[(dyy * dw + dxx) as usize] = tmp[(row * w + col) as usize];
            }
        }
    };
    if let Some(win) = c.windows.iter_mut().find(|w| w.id == dst) {
        put(&mut win.buf, win.w as i32, win.h as i32);
    } else if let Some(pm) = c.pixmaps.iter_mut().find(|p| p.id == dst) {
        put(&mut pm.buf, pm.w as i32, pm.h as i32);
    }
}

/// Present the given (mapped) window to the real framebuffer + verify a sample
/// pixel for the bring-up log. Reuses the app-graphics XRGB blit (as DOOM does).
fn present(c: &XConn, id: u32) {
    // The anchor for the shared placement transform: the largest mapped window on
    // the processing connection or in the table. Computed HERE — with the table
    // lock taken and released before present_win — because present_win runs on the
    // foreign path below while the table lock is held; taking XCONNS inside it
    // again self-deadlocks the presenting task, pump_mouse (task 0) then spins on
    // the lock forever and the mouse goes dead (the bt1/bt2 regression).
    let mut best: Option<(i16, i16, u16, u16)> = c.windows.iter()
        .filter(|w| w.mapped && w.w > 1 && w.h > 1)
        .map(|w| (w.x, w.y, w.w, w.h))
        .max_by_key(|(_, _, w, h)| *w as u32 * *h as u32);
    {
        let t = XCONNS.lock();
        for conn in t.iter().flatten() {
            for w in conn.windows.iter().filter(|w| w.mapped && w.w > 1 && w.h > 1) {
                if best.map(|(_, _, bw, bh)| (w.w as u32 * w.h as u32) > (bw as u32 * bh as u32)).unwrap_or(true) {
                    best = Some((w.x, w.y, w.w, w.h));
                }
            }
        }
    }
    ANCHOR.lock().clone_from(&best);
    if X_WINDOWED.load(core::sync::atomic::Ordering::Relaxed) {
        compose_windowed(c);
        return;
    }
    if c.windows.iter().any(|w| w.id == id) {
        present_win(c.windows.iter().find(|w| w.id == id && w.mapped), id);
        return;
    }
    // Foreign drawable: find its owner (the processing connection is out of the
    // table, so no aliasing) and present THAT window.
    let t = XCONNS.lock();
    for conn in t.iter().flatten() {
        if conn.windows.iter().any(|w| w.id == id) {
            present_win(conn.windows.iter().find(|w| w.id == id && w.mapped), id);
            return;
        }
    }
}

/// Windowed mode: what the desktop shows in the client's frame. The MAIN window
/// (the largest mapped one: the browser window) fills the frame; every other
/// mapped window (a tooltip, a menu, a drop-down) is drawn on top of it at its
/// X position relative to the main window, in creation order, clipped to it.
///
/// It used to be "the window that painted last": the first tooltip took the
/// browser's place in the frame (a white frame with one line of text), clicks
/// were sent to the tooltip, and a maximise resized the tooltip (BUG-097).
/// `c` is the connection being processed (it is out of the table).
fn compose_windowed(c: &XConn) {
    let t = XCONNS.lock();
    let all = c.windows.iter().chain(t.iter().flatten().flat_map(|x| x.windows.iter()))
        .filter(|w| w.mapped && w.w > 1 && w.h > 1 && w.buf.len() == w.w as usize * w.h as usize);
    let Some(main) = all.clone().max_by_key(|w| (w.w as u32 * w.h as u32, core::cmp::Reverse(w.id))) else {
        return;
    };
    let (mw, mh) = (main.w as usize, main.h as usize);
    let mut r = RETAINED_WINDOW.lock();
    match r.as_mut() {
        Some((w, h, buf)) if buf.len() == main.buf.len() => {
            *w = mw;
            *h = mh;
            buf.copy_from_slice(&main.buf);
        }
        _ => *r = Some((mw, mh, main.buf.clone())),
    }
    if let Some((_, _, buf)) = r.as_mut() {
        for p in all.filter(|w| w.id != main.id) {
            let (ox, oy) = (p.x as i32 - main.x as i32, p.y as i32 - main.y as i32);
            let (x0, x1) = (ox.max(0), (ox + p.w as i32).min(mw as i32));
            if x0 >= x1 {
                continue;
            }
            let n = (x1 - x0) as usize;
            for sy in 0..p.h as i32 {
                let dy = oy + sy;
                if dy < 0 || dy >= mh as i32 {
                    continue;
                }
                let s0 = sy as usize * p.w as usize + (x0 - ox) as usize;
                let d0 = dy as usize * mw + x0 as usize;
                buf[d0..d0 + n].copy_from_slice(&p.buf[s0..s0 + n]);
            }
        }
    }
    RETAINED_ID.store(main.id, core::sync::atomic::Ordering::Relaxed);
    X_DIRTY.store(true, core::sync::atomic::Ordering::Relaxed);
    keylat::drawn();
    crate::sched::want_desktop();
}

/// The current placement anchor (x, y, w, h), refreshed by present() BEFORE the
/// window table is locked for the blit.
static ANCHOR: Mutex<Option<(i16, i16, u16, u16)>> = Mutex::new(None);

fn present_win(win: Option<&XWindow>, id: u32) {
    if let Some(win) = win {
        // Windowed mode: the desktop compositor draws the frame + pulls the pixels, so
        // skip the fullscreen blit (it would fight the compositor). RETAIN the pixels so
        // the app shows as a framed window and keeps showing after a boot-run client
        // exits, and flag a repaint.
        if X_WINDOWED.load(core::sync::atomic::Ordering::Relaxed) {
            if win.w > 1 && win.h > 1 {
                // REUSE the retained buffer instead of cloning ~1.9 MB per present:
                // 30 presents/s of alloc+free churn fragments the kernel heap until
                // a 256 KB PutImage buffer cannot be placed (the dt6 OOM panic).
                {
                    let mut r = RETAINED_WINDOW.lock();
                    match r.as_mut() {
                        Some((w, h, buf)) if buf.len() == win.buf.len() => {
                            *w = win.w as usize;
                            *h = win.h as usize;
                            buf.copy_from_slice(&win.buf);
                        }
                        _ => *r = Some((win.w as usize, win.h as usize, win.buf.clone())),
                    }
                }
                // Remember WHICH window those pixels belong to, so the desktop's click
                // and motion routing reaches the window the user is looking at instead
                // of whichever one happens to be biggest.
                RETAINED_ID.store(id, core::sync::atomic::Ordering::Relaxed);
            }
            X_DIRTY.store(true, core::sync::atomic::Ordering::Relaxed);
            keylat::drawn();
            crate::sched::want_desktop();
        } else {
            // ONE shared transform for every window: the ANCHOR (largest mapped
            // window on any connection — chrome's browser toplevel) is centred and
            // integer-scaled by screen_place; every other window is placed with the
            // SAME scale at its X-geometry position relative to the anchor. This is
            // what makes a menu popup open AT its button instead of floating in a
            // corner as an unrelated blob.
            let anchor = *ANCHOR.lock();
            let placed = anchor.and_then(|(ax, ay, aw, ah)| {
                crate::screen_place(aw as usize, ah as usize)
                    .map(|(adx, ady, s)| (
                        adx as i64 + (win.x as i64 - ax as i64) * s as i64,
                        ady as i64 + (win.y as i64 - ay as i64) * s as i64,
                        s,
                    ))
            });
            // Anchor placement re-enabled: the bisect cleared it (the bt1/bt2
            // regression was the present() self-deadlock, fixed separately by
            // precomputing ANCHOR before the table lock). Popups now open AT
            // their X position relative to the browser window.
            match placed {
                Some((dx, dy, sc)) => {
                    crate::screen_present_xrgb_at(&win.buf, win.w as usize, win.h as usize, dx, dy, sc);
                    note_presented(id, win.w, win.h, dx, dy, sc);
                }
                None => {
                    crate::screen_present_xrgb(&win.buf, win.w as usize, win.h as usize);
                    if let Some((dx, dy, sc)) = crate::screen_place(win.w as usize, win.h as usize) {
                        note_presented(id, win.w, win.h, dx as i64, dy as i64, sc);
                    }
                }
            }
        }
        let ctr = (win.h as usize / 2) * win.w as usize + win.w as usize / 2;
        let sample = win.buf.get(ctr).copied().unwrap_or(0);
        trace(format_args!("present id={id:#x} {}x{} centre-pixel={sample:#08x}", win.w, win.h));
    }
}

/// Set when a windowed X client repainted, so the desktop knows to recomposite it.
pub static X_DIRTY: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
pub fn take_dirty() -> bool { X_DIRTY.swap(false, core::sync::atomic::Ordering::Relaxed) }

/// Build an 8-byte reply header (reply type 1, sequence, length-in-units field).
/// `extra_units` is beyond the fixed 32-byte reply body — 0 for all simple replies
/// (X replies are at minimum 32 bytes). Returns a 32-byte buffer to fill + emit.
fn reply_header(c: &XConn, extra_units: u32) -> Vec<u8> {
    let mut r = alloc::vec![0u8; 32 + extra_units as usize * 4];
    r[0] = 1; // reply
    // r[1] = detail-specific (0 here)
    put16(c, &mut r, 2, c.seq); // sequence number
    put32(c, &mut r, 4, extra_units); // reply length in 4-byte units beyond 32
    r
}

/// Ensure the reply is at least 32 bytes (X minimum). No-op here (already sized).
fn pad_reply(_r: &mut [u8]) {}

/// The connection-setup success reply. One screen, depth 24, one TrueColor visual.
fn setup_reply(swap: bool, rid_base: u32) -> Vec<u8> {
    let vendor = b"EuroOS X";
    let vlen = vendor.len();
    // Additional data (after the 8-byte header):
    let mut a: Vec<u8> = Vec::new();
    p32(&mut a, swap, 11_000_000); // release-number
    p32(&mut a, swap, rid_base); // resource-id-base (PER CONNECTION — see open())
    p32(&mut a, swap, RID_MASK); // resource-id-mask
    p32(&mut a, swap, 256); // motion-buffer-size
    p16(&mut a, swap, vlen as u16); // vendor length
    p16(&mut a, swap, 65535); // maximum-request-length
    a.push(1); // number of screens
    a.push(1); // number of pixmap formats
    a.push(0); // image-byte-order: LSBFirst
    a.push(0); // bitmap-format-bit-order: LeastSignificant
    a.push(32); // bitmap-format-scanline-unit
    a.push(32); // bitmap-format-scanline-pad
    a.push(8); // min-keycode
    a.push(255); // max-keycode
    a.extend_from_slice(&[0, 0, 0, 0]); // unused
    a.extend_from_slice(vendor);
    while a.len() % 4 != 0 {
        a.push(0); // pad vendor to 4
    }
    // Pixmap format (8 bytes): depth 24, bpp 32, scanline-pad 32.
    a.extend_from_slice(&[24, 32, 32, 0, 0, 0, 0, 0]);
    // Screen (40 bytes fixed + depths).
    p32(&mut a, swap, ROOT_WINDOW); // root
    p32(&mut a, swap, DEFAULT_CMAP); // default-colormap
    p32(&mut a, swap, 0x00ff_ffff); // white-pixel
    p32(&mut a, swap, 0x0000_0000); // black-pixel
    p32(&mut a, swap, 0); // current-input-masks
    p16(&mut a, swap, screen_w()); // width in pixels
    p16(&mut a, swap, screen_h()); // height in pixels
    p16(&mut a, swap, (screen_w() as u32 * 254 / 960) as u16); // width in mm (~96 dpi)
    p16(&mut a, swap, (screen_h() as u32 * 254 / 960) as u16); // height in mm
    p16(&mut a, swap, 1); // min-installed-maps
    p16(&mut a, swap, 1); // max-installed-maps
    p32(&mut a, swap, ROOT_VISUAL); // root-visual
    a.push(0); // backing-stores: Never
    a.push(0); // save-unders: False
    a.push(24); // root-depth
    a.push(1); // number of depths
    // Depth (8 bytes fixed + visuals): depth 24, 1 visual.
    a.push(24); // depth
    a.push(0); // unused
    p16(&mut a, swap, 1); // number of visuals
    a.extend_from_slice(&[0, 0, 0, 0]); // unused
    // Visual (24 bytes): TrueColor.
    p32(&mut a, swap, ROOT_VISUAL); // visual-id
    a.push(4); // class: TrueColor
    a.push(8); // bits-per-rgb-value
    p16(&mut a, swap, 256); // colormap-entries
    p32(&mut a, swap, 0x00ff_0000); // red-mask
    p32(&mut a, swap, 0x0000_ff00); // green-mask
    p32(&mut a, swap, 0x0000_00ff); // blue-mask
    a.extend_from_slice(&[0, 0, 0, 0]); // unused

    debug_assert!(a.len() % 4 == 0);
    let units = (a.len() / 4) as u16;
    // 8-byte header.
    let mut r: Vec<u8> = Vec::with_capacity(8 + a.len());
    r.push(1); // success
    r.push(0); // unused
    p16(&mut r, swap, 11); // protocol-major-version
    p16(&mut r, swap, 0); // protocol-minor-version
    p16(&mut r, swap, units); // length of additional data in 4-byte units
    r.extend_from_slice(&a);
    r
}

// ── Byte helpers (respect the client's byte order) ──────────────────────────

fn pad4(n: usize) -> usize { (n + 3) & !3 }

/// Read a u16/u32 from a request slice at `off`, respecting the client's byte order.
fn ru16(c: &XConn, req: &[u8], off: usize) -> u16 {
    if off + 2 > req.len() { return 0; }
    let b = [req[off], req[off + 1]];
    if c.swap { u16::from_be_bytes(b) } else { u16::from_le_bytes(b) }
}
fn ru32(c: &XConn, req: &[u8], off: usize) -> u32 {
    if off + 4 > req.len() { return 0; }
    let b = [req[off], req[off + 1], req[off + 2], req[off + 3]];
    if c.swap { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) }
}

fn rd16(c: &XConn, off: usize) -> u16 {
    let b = [c.inbuf[off], c.inbuf[off + 1]];
    if c.swap { u16::from_be_bytes(b) } else { u16::from_le_bytes(b) }
}

fn put16(c: &XConn, buf: &mut [u8], off: usize, v: u16) {
    let b = if c.swap { v.to_be_bytes() } else { v.to_le_bytes() };
    buf[off..off + 2].copy_from_slice(&b);
}
fn put32(c: &XConn, buf: &mut [u8], off: usize, v: u32) {
    let b = if c.swap { v.to_be_bytes() } else { v.to_le_bytes() };
    buf[off..off + 4].copy_from_slice(&b);
}
fn wr32(c: &XConn, buf: &mut [u8], off: usize, v: u32) { put32(c, buf, off, v) }

fn p16(v: &mut Vec<u8>, swap: bool, x: u16) {
    v.extend_from_slice(&if swap { x.to_be_bytes() } else { x.to_le_bytes() });
}
fn p32(v: &mut Vec<u8>, swap: bool, x: u32) {
    v.extend_from_slice(&if swap { x.to_be_bytes() } else { x.to_le_bytes() });
}
