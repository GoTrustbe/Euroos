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

/// X-connection fds live in their own space so they don't collide with VFS (small),
/// AF_INET (500+) or AF_UNIX (600+) fds.
pub const XCONN_FD_BASE: u64 = 700;
const MAX_XCONN: usize = 8;

/// Default screen geometry reported in the setup (refined to the real framebuffer
/// once window mapping is wired to the compositor).
const SCREEN_W: u16 = 1280;
const SCREEN_H: u16 = 800;

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
}

/// A graphics context — for the first rendering rung we only track the fill colour.
struct XGc {
    id: u32,
    fg: u32,
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
        XCONNS.lock()[(fd - XCONN_FD_BASE) as usize] = None;
    }
}

/// The client wrote `data` — feed the protocol state machine, producing replies.
pub fn write(fd: u64, data: &[u8]) -> u64 {
    // Take the connection OUT of the table while processing its requests, so the
    // drawing ops can reach SIBLING connections: X resource ids are server-global
    // (chrome's viz component paints, over its own connection, into the window the
    // browser connection created), and holding the table lock across processing
    // made that lookup impossible.
    let idx = (fd - XCONN_FD_BASE) as usize;
    let mut c = match XCONNS.lock().get_mut(idx).and_then(|s| s.take()) {
        Some(c) => c,
        None => return (-9i64) as u64, // -EBADF
    };
    c.inbuf.extend_from_slice(data);
    process(&mut c);
    if let Some(slot) = XCONNS.lock().get_mut(idx) {
        *slot = Some(c);
    }
    data.len() as u64
}

/// The client is reading — drain up to `max` bytes of queued replies/events.
pub fn read(fd: u64, max: usize) -> Vec<u8> {
    let mut t = XCONNS.lock();
    let c = match t.get_mut((fd - XCONN_FD_BASE) as usize).and_then(|s| s.as_mut()) {
        Some(c) => c,
        None => return Vec::new(),
    };
    let n = max.min(c.outbuf.len());
    if n > 0 {
        trace(format_args!("client read {n} B ({} left queued)", c.outbuf.len() - n));
    }
    c.outbuf.drain(0..n).collect()
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
    MOD_STATE.load(core::sync::atomic::Ordering::Relaxed)
        | if crate::mouse::left_down() { 0x100 } else { 0 }
}

/// Pump REAL keyboard input into X key events. Pops PS/2 scancodes and delivers a
/// KeyPress(2)/KeyRelease(3) to every connection whose mapped window selected that
/// event (X keycode = scancode + 8). Called from the run_glibc wait loop while an X
/// client is up — this is how live hardware input reaches an X window (vs. the
/// injected self-test events). No-op (pops nothing) unless a window wants keys.
pub fn pump_keyboard() {
    // IRQ-safe: called from task 0 (desktop) while the client's read/process spins on
    // XCONNS with IF=0 — a preemption holding this lock would deadlock (BUG-007 class).
    x86_64::instructions::interrupts::without_interrupts(|| {
        let wants = {
            let t = XCONNS.lock();
            t.iter().flatten().any(|c| c.windows.iter().any(|w| w.mapped && w.event_mask & 0x3 != 0))
        };
        if !wants {
            return;
        }
        while let Some(sc) = crate::ps2::poll_scancode() {
            let pressed = sc & 0x80 == 0;
            let code = sc & 0x7f;
            // Track the modifiers ourselves: the state field of EVERY event carries
            // them, and a key event only means "A" instead of "a" because of it.
            let bit: u16 = match code {
                0x2a | 0x36 => 0x1, // Shift_L / Shift_R
                0x1d => 0x4,        // Control_L
                0x38 => 0x8,        // Alt_L (mod1)
                _ => 0,
            };
            if bit != 0 {
                if pressed {
                    MOD_STATE.fetch_or(bit, core::sync::atomic::Ordering::Relaxed);
                } else {
                    MOD_STATE.fetch_and(!bit, core::sync::atomic::Ordering::Relaxed);
                }
            }
            let keycode = code + 8; // X keycode = PS/2 scancode + 8
            let want: u32 = if pressed { 0x1 } else { 0x2 }; // KeyPress / KeyRelease mask
            let kind: u8 = if pressed { 2 } else { 3 };
            let (mx, my) = crate::mouse::pos();
            #[allow(unused_assignments)]
            let mut t = XCONNS.lock();
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
                t = XCONNS.lock();
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
    ((c.rid_base >> 21) as usize).saturating_sub(1)
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
    let sels: Vec<usize> = SELECTIONS.lock().iter()
        .filter(|(w, _, m)| *w == win && (want == 0 || m & want != 0))
        .map(|(_, c, _)| *c)
        .collect();
    let mut sent = 0;
    let mut t = XCONNS.lock();
    for ci in sels {
        if let Some(Some(conn)) = t.get_mut(ci).map(|s| s.as_mut()) {
            send_input(conn, kind, detail, win, rx, ry, lx, ly);
            sent += 1;
        }
    }
    sent
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
fn note_presented(id: u32, w: u16, h: u16) {
    let (dx, dy, sc) = match crate::screen_place(w as usize, h as usize) {
        Some(p) => p,
        None => return,
    };
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
    let t = PRESENTED.lock();
    t.iter()
        .filter(|(_, dx, dy, sc, w, h, _)| {
            px >= *dx && px < dx + w * sc && py >= *dy && py < dy + h * sc
        })
        .max_by_key(|r| r.6)
        .map(|(id, dx, dy, sc, _, _, _)| (*id, *dx, *dy, *sc))
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
pub fn pump_mouse() {
    // ButtonPressMask(0x4) or PointerMotionMask(0x40) selected by any mapped window?
    let wants = {
        let t = XCONNS.lock();
        t.iter().flatten().any(|c| c.windows.iter().any(|w| w.mapped && w.event_mask & 0x44 != 0))
    };
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
            let mut t = XCONNS.lock();
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
        let mut t = XCONNS.lock();
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
        let mut t = XCONNS.lock();
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
    trace(format_args!("deliver_button local=({lx},{ly})"));
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
        let mut t = XCONNS.lock();
        let shown = RETAINED_ID.load(core::sync::atomic::Ordering::Relaxed);
        if shown != 0 {
            if let Some(ci) = conn_of_window(&t[..], shown) {
                if let Some(conn) = t[ci].as_mut() {
                    let (wx, wy) = conn.windows.iter().find(|w| w.id == shown)
                        .map(|w| (w.x, w.y)).unwrap_or((0, 0));
                    send_input(conn, kind, detail, shown, lx + wx, ly + wy, lx, ly);
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

/// US-QWERTY X keysyms for a keycode (which is PS/2 set-1 scancode + 8, matching the
/// standard X keycodes): returns (unshifted, shifted). NoSymbol (0,0) if unmapped.
fn keysym_for(kc: u8) -> (u32, u32) {
    // Letters: lowercase = ASCII, shifted = uppercase.
    let letter = |c: u8| (c as u32, (c - 0x20) as u32);
    match kc {
        9 => (0xff1b, 0xff1b),   // Escape
        10 => (0x31, 0x21), 11 => (0x32, 0x40), 12 => (0x33, 0x23), 13 => (0x34, 0x24),
        14 => (0x35, 0x25), 15 => (0x36, 0x5e), 16 => (0x37, 0x26), 17 => (0x38, 0x2a),
        18 => (0x39, 0x28), 19 => (0x30, 0x29), // 1..9 0
        20 => (0x2d, 0x5f), 21 => (0x3d, 0x2b), // - =
        22 => (0xff08, 0xff08), // BackSpace
        23 => (0xff09, 0xff09), // Tab
        24 => letter(b'q'), 25 => letter(b'w'), 26 => letter(b'e'), 27 => letter(b'r'),
        28 => letter(b't'), 29 => letter(b'y'), 30 => letter(b'u'), 31 => letter(b'i'),
        32 => letter(b'o'), 33 => letter(b'p'),
        34 => (0x5b, 0x7b), 35 => (0x5d, 0x7d), // [ ]
        36 => (0xff0d, 0xff0d), // Return
        37 => (0xffe3, 0xffe3), // Control_L
        38 => letter(b'a'), 39 => letter(b's'), 40 => letter(b'd'), 41 => letter(b'f'),
        42 => letter(b'g'), 43 => letter(b'h'), 44 => letter(b'j'), 45 => letter(b'k'),
        46 => letter(b'l'),
        47 => (0x3b, 0x3a), 48 => (0x27, 0x22), 49 => (0x60, 0x7e), // ; ' `
        50 => (0xffe1, 0xffe1), // Shift_L
        51 => (0x5c, 0x7c), // backslash |
        52 => letter(b'z'), 53 => letter(b'x'), 54 => letter(b'c'), 55 => letter(b'v'),
        56 => letter(b'b'), 57 => letter(b'n'), 58 => letter(b'm'),
        59 => (0x2c, 0x3c), 60 => (0x2e, 0x3e), 61 => (0x2f, 0x3f), // , . /
        62 => (0xffe2, 0xffe2), // Shift_R
        64 => (0xffe9, 0xffe9), // Alt_L
        65 => (0x20, 0x20), // space
        66 => (0xffe5, 0xffe5), // Caps_Lock
        _ => (0, 0),
    }
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
            let mut r = reply_header(c, 0);
            // 24 extra bytes already zero: present(1)=0, major(1), first-event(1),
            // first-error(1), then 20 unused. Nothing more to set.
            pad_reply(&mut r);
            c.outbuf.extend_from_slice(&r);
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
        // InternAtom (16): hand back a synthetic atom id (seq-based, nonzero).
        16 => {
            let mut r = reply_header(c, 0);
            wr32(c, &mut r, 8, 0x1000 + c.seq as u32); // atom
            pad_reply(&mut r);
            c.outbuf.extend_from_slice(&r);
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
            let w = if raw_w >= 8192 { 400 } else { raw_w.clamp(1, SCREEN_W) };
            let h = if raw_h >= 8192 { 260 } else { raw_h.clamp(1, SCREEN_H) };
            let buf = alloc::vec![0xff20_2020u32; w as usize * h as usize]; // opaque dark
            // CreateWindow can carry an event-mask too (value-mask @28, CWEventMask=0x800).
            let em = win_event_mask(c, req, 28, 32);
            c.windows.push(XWindow { id, x, y, w, h, mapped: false, event_mask: em, buf });
            if em != 0 {
                select_events(id, conn_index(c), em);
            }
            trace(format_args!("CreateWindow id={id:#x} {w}x{h} @({x},{y}) mask={em:#x}"));
        }
        // CreateGC(55): cid@4, drawable@8, value-mask@12, values@16.
        55 => {
            let id = ru32(c, req, 4);
            let fg = gc_foreground(c, req, 12, 16).unwrap_or(0);
            c.gcs.push(XGc { id, fg });
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
            for bit in 0..7 {
                if mask & (1 << bit) != 0 {
                    let v = ru32(c, req, voff);
                    // Ignore an absurd (unconstrained, >=8192) width/height — WM-less GTK
                    // asks for its full unconstrained size; honouring it would make a
                    // giant window. Keep the current size for those.
                    match bit {
                        2 if v < 8192 => nw = Some((v as u16).clamp(1, SCREEN_W)),
                        3 if v < 8192 => nh = Some((v as u16).clamp(1, SCREEN_H)),
                        _ => {}
                    }
                    voff += 4;
                }
            }
            let mut notify = None;
            if let Some(win) = c.windows.iter_mut().find(|w| w.id == id) {
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
            trace(format_args!("ConfigureWindow id={id:#x} -> {:?}x{:?}", nw, nh));
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
            let w = ru16(c, req, 12).clamp(1, SCREEN_W);
            let h = ru16(c, req, 14).clamp(1, SCREEN_H);
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
        // GetSelectionOwner(23): report None (0) — no selection owner. GTK checks the
        // clipboard/WM/CM selections at startup; None is a valid answer.
        23 => {
            let mut r = reply_header(c, 0);
            wr32(c, &mut r, 8, 0); // owner = None
            c.outbuf.extend_from_slice(&r);
        }
        // GetGeometry(14): drawable@4. Reply: depth in r[1], root, x,y,w,h,border.
        14 => {
            let id = ru32(c, req, 4);
            let (x, y, w, h) = c.windows.iter().find(|w| w.id == id)
                .map(|win| (win.x, win.y, win.w, win.h))
                .unwrap_or((0, 0, SCREEN_W, SCREEN_H));
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
        // QueryPointer(38): pointer on root, at the live mouse position, no buttons.
        38 => {
            let (mx, my) = crate::mouse::pos();
            let mut r = reply_header(c, 0);
            r[1] = 1; // same-screen = true
            wr32(c, &mut r, 8, ROOT_WINDOW);   // root
            wr32(c, &mut r, 12, 0);            // child = None
            put16(c, &mut r, 16, mx as u16);   // root-x
            put16(c, &mut r, 18, my as u16);   // root-y
            put16(c, &mut r, 20, mx as u16);   // win-x
            put16(c, &mut r, 22, my as u16);   // win-y
            put16(c, &mut r, 24, 0);           // mask (buttons/mods)
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
        // GetKeyboardMapping(101): first-keycode@4, count@5. Return a US-QWERTY keymap
        // (2 keysyms/keycode: unshifted, shifted). Our keycodes are scancode+8, which
        // match the standard X keycodes, so a toolkit maps keycode->keysym->character.
        101 => {
            let first = req.get(4).copied().unwrap_or(8);
            let count = req.get(5).copied().unwrap_or(0) as usize;
            const N: usize = 2; // keysyms per keycode
            let mut r = reply_header(c, (count * N) as u32);
            r[1] = N as u8;
            for i in 0..count {
                let kc = first.wrapping_add(i as u8);
                let (lo, hi) = keysym_for(kc);
                let base = 32 + i * N * 4;
                r[base..base + 4].copy_from_slice(&lo.to_le_bytes());
                r[base + 4..base + 8].copy_from_slice(&hi.to_le_bytes());
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
            // modifier index: 0 Shift, 1 Lock, 2 Control, 3 Mod1(Alt) ...
            let mods: [(usize, u8); 4] = [(0, 50), (1, 66), (2, 37), (3, 64)]; // Shift_L, Caps, Ctrl_L, Alt_L
            for (m, kc) in mods {
                r[32 + m * KPM] = kc; // first keycode for that modifier
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
        // QueryFont(47): a minimal font reply so a core-font query doesn't hang. Reply
        // has 7 extra fixed units (min/max bounds + counts, all zero = an empty font).
        47 => {
            let r = reply_header(c, 7);
            c.outbuf.extend_from_slice(&r);
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
                c.windows.retain(|w| w.id != id);
                SELECTIONS.lock().retain(|(w, _, _)| *w != id);
            } else if let Some(win) = c.windows.iter_mut().find(|w| w.id == id) {
                win.mapped = false;
            }
            trace(format_args!("{} id={id:#x}", if opcode == 4 { "DestroyWindow" } else { "UnmapWindow" }));
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
        _ => {}
    }
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
    // state@28: the live modifier + button mask (shift/ctrl/alt, button 1 held).
    e[28..30].copy_from_slice(&mod_state().to_le_bytes());
    e[30] = 1; // same-screen
    c.outbuf.extend_from_slice(&e);
    // The queue length answers the question a click that changes nothing raises: did
    // the client ever COLLECT the event? A number that keeps growing means the events
    // are piling up unread, and no amount of aiming at the right pixel would help.
    trace(format_args!("-> input kind={kind} detail={detail} window={window:#x} at({ex},{ey}) queued={} B",
        c.outbuf.len()));
    // Events piling up unread: the client is not collecting them. Ask the next waits to
    // say what they are waiting on and what we report as ready — the only way to tell
    // "chrome never watches this fd" from "we tell it the fd is empty" from "we say
    // ready and chrome ignores it".
    // Re-arm with a cooldown instead of once-only: the first backlog often happens
    // during hover (the thread is merely busy), and the dump that matters is the one
    // taken while the click sits unread minutes later.
    if c.outbuf.len() >= 128 {
        let now = crate::interrupts::ticks();
        let last = STALL_LAST_DUMP.load(core::sync::atomic::Ordering::Relaxed);
        if now >= last + 3000
            && STALL_LAST_DUMP.compare_exchange(last, now,
                core::sync::atomic::Ordering::Relaxed, core::sync::atomic::Ordering::Relaxed).is_ok()
        {
            crate::serial_println!("[xserver] {} B of input queued unread on connection {} — who should be reading it?",
                c.outbuf.len(), c.rid_base >> 21);
            crate::ring3::arm_wait_diag(30);
            crate::ring3::dump_threads_now("input events queued unread");
            crate::ring3::dump_main_syscalls();
            crate::ring3::dump_futex_state();
            crate::ring3::dump_syscall_histogram();
            // From here the profile is about the STALL, not about startup.
            crate::ring3::reset_rip_profile();
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

fn present_win(win: Option<&XWindow>, id: u32) {
    if let Some(win) = win {
        // Windowed mode: the desktop compositor draws the frame + pulls the pixels, so
        // skip the fullscreen blit (it would fight the compositor). RETAIN the pixels so
        // the app shows as a framed window and keeps showing after a boot-run client
        // exits, and flag a repaint.
        if X_WINDOWED.load(core::sync::atomic::Ordering::Relaxed) {
            if win.w > 1 && win.h > 1 {
                *RETAINED_WINDOW.lock() = Some((win.w as usize, win.h as usize, win.buf.clone()));
                // Remember WHICH window those pixels belong to, so the desktop's click
                // and motion routing reaches the window the user is looking at instead
                // of whichever one happens to be biggest.
                RETAINED_ID.store(id, core::sync::atomic::Ordering::Relaxed);
            }
            X_DIRTY.store(true, core::sync::atomic::Ordering::Relaxed);
        } else {
            crate::screen_present_xrgb(&win.buf, win.w as usize, win.h as usize);
            note_presented(id, win.w, win.h);
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
    p16(&mut a, swap, SCREEN_W); // width in pixels
    p16(&mut a, swap, SCREEN_H); // height in pixels
    p16(&mut a, swap, (SCREEN_W as u32 * 254 / 960) as u16); // width in mm (~96 dpi)
    p16(&mut a, swap, (SCREEN_H as u32 * 254 / 960) as u16); // height in mm
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
