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

struct XConn {
    inbuf: Vec<u8>,   // accumulated request bytes not yet consumed
    outbuf: Vec<u8>,  // reply/event bytes waiting to be read()
    state: State,
    seq: u16,         // last request's sequence number (server-side counter)
    swap: bool,       // client is big-endian (byte-swap multi-byte fields)
    windows: Vec<XWindow>,
    gcs: Vec<XGc>,
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
            *s = Some(XConn { inbuf: Vec::new(), outbuf: Vec::new(), state: State::PreSetup, seq: 0, swap: false, windows: Vec::new(), gcs: Vec::new() });
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
    let mut t = XCONNS.lock();
    let c = match t.get_mut((fd - XCONN_FD_BASE) as usize).and_then(|s| s.as_mut()) {
        Some(c) => c,
        None => return (-9i64) as u64, // -EBADF
    };
    c.inbuf.extend_from_slice(data);
    process(c);
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
    c.outbuf.drain(0..n).collect()
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

/// Pump REAL keyboard input into X key events. Pops PS/2 scancodes and delivers a
/// KeyPress(2)/KeyRelease(3) to every connection whose mapped window selected that
/// event (X keycode = scancode + 8). Called from the run_glibc wait loop while an X
/// client is up — this is how live hardware input reaches an X window (vs. the
/// injected self-test events). No-op (pops nothing) unless a window wants keys.
pub fn pump_keyboard() {
    // Fast path: only touch the scancode ring if some window actually wants keys.
    let wants = {
        let t = XCONNS.lock();
        t.iter().flatten().any(|c| c.windows.iter().any(|w| w.mapped && w.event_mask & 0x3 != 0))
    };
    if !wants {
        return;
    }
    while let Some(sc) = crate::ps2::poll_scancode() {
        let pressed = sc & 0x80 == 0;
        let keycode = (sc & 0x7f) + 8; // X keycode = PS/2 scancode + 8
        let want: u32 = if pressed { 0x1 } else { 0x2 }; // KeyPress / KeyRelease mask
        let kind: u8 = if pressed { 2 } else { 3 };
        let mut t = XCONNS.lock();
        for conn in t.iter_mut().flatten() {
            let wid = conn
                .windows
                .iter()
                .find(|w| w.mapped && w.event_mask & want != 0)
                .map(|w| w.id);
            if let Some(wid) = wid {
                send_input(conn, kind, keycode, wid, 0, 0);
            }
        }
    }
}

/// Pump REAL mouse input into X ButtonPress events. Consumes a left-button press
/// latch from the mouse driver and delivers ButtonPress(button 1) to a window that
/// selected it, with the click position. Called from the run_glibc wait loop.
pub fn pump_mouse() {
    let wants = {
        let t = XCONNS.lock();
        t.iter().flatten().any(|c| c.windows.iter().any(|w| w.mapped && w.event_mask & 0x4 != 0))
    };
    if !wants {
        return;
    }
    if let Some((mx, my)) = crate::mouse::take_press() {
        let mut t = XCONNS.lock();
        for conn in t.iter_mut().flatten() {
            let wid = conn
                .windows
                .iter()
                .find(|w| w.mapped && w.event_mask & 0x4 != 0)
                .map(|w| w.id);
            if let Some(wid) = wid {
                send_input(conn, 4, 1, wid, mx as i16, my as i16); // ButtonPress, button 1
            }
        }
    }
}

fn trace(args: core::fmt::Arguments) {
    if TRACE.load(core::sync::atomic::Ordering::Relaxed) {
        crate::serial_println!("[xserver] {args}");
    }
}

// ── Protocol ────────────────────────────────────────────────────────────────

fn process(c: &mut XConn) {
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
        let reply = setup_reply(c.swap);
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
        let req: Vec<u8> = c.inbuf.drain(0..req_len).collect();
        c.seq = c.seq.wrapping_add(1);
        handle_request(c, opcode, detail, &req);
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
            wr32(c, &mut r, 8, ROOT_WINDOW); // focus = root
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
            let w = ru16(c, req, 16).max(1);
            let h = ru16(c, req, 18).max(1);
            let buf = alloc::vec![0xff20_2020u32; w as usize * h as usize]; // opaque dark
            // CreateWindow can carry an event-mask too (value-mask @28, CWEventMask=0x800).
            let em = win_event_mask(c, req, 28, 32);
            c.windows.push(XWindow { id, x, y, w, h, mapped: false, event_mask: em, buf });
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
            trace(format_args!("ChangeWindowAttributes id={id:#x} event-mask={em:#x}"));
        }
        // MapWindow(8): window@4 -> visible; present it; then deliver the events the
        // window asked for (Expose always; test key/button when injection is enabled).
        8 => {
            let id = ru32(c, req, 4);
            let (mask, w, h) = match c.windows.iter_mut().find(|w| w.id == id) {
                Some(win) => { win.mapped = true; (win.event_mask, win.w, win.h) }
                None => (0, 0, 0),
            };
            trace(format_args!("MapWindow id={id:#x} mask={mask:#x}"));
            present(c, id);
            const EXPOSURE: u32 = 0x8000;
            const KEY_PRESS: u32 = 0x0001;
            const BUTTON_PRESS: u32 = 0x0004;
            if mask & EXPOSURE != 0 {
                send_expose(c, id, w, h);
            }
            if INJECT_TEST_INPUT.load(core::sync::atomic::Ordering::Relaxed) {
                if mask & KEY_PRESS != 0 {
                    send_input(c, 2, 38, id, 100, 60); // KeyPress, keycode 38 ('a')
                }
                if mask & BUTTON_PRESS != 0 {
                    send_input(c, 4, 1, id, 100, 60); // ButtonPress, button 1
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
                    "PutImage quadrants TL={:#08x} TR={:#08x} BL={:#08x} BR={:#08x}",
                    at(ww / 4, wh / 4), at(ww * 3 / 4, wh / 4), at(ww / 4, wh * 3 / 4), at(ww * 3 / 4, wh * 3 / 4)
                ));
            }
            present(c, draw);
        }
        // Everything else (no reply): acknowledged by consuming the request.
        _ => {}
    }
}

/// Copy a ZPixmap (32-bpp, LSBFirst) into the target window buffer, clipped.
fn put_image(c: &mut XConn, draw: u32, dst_x: i32, dst_y: i32, w: usize, h: usize, data: &[u8]) {
    if let Some(win) = c.windows.iter_mut().find(|win| win.id == draw) {
        let ww = win.w as i32;
        let wh = win.h as i32;
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
                win.buf[(dy * ww + dx) as usize] = 0xff00_0000 | (px & 0x00ff_ffff);
            }
        }
    }
}

/// When set, MapWindow also delivers a synthetic KeyPress + ButtonPress to a window
/// that selected them — so event delivery is testable without live hardware input.
/// (Real apps get only legitimate events; this is gated for the gxevent self-test.)
pub static INJECT_TEST_INPUT: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

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

/// Queue a 32-byte input event (KeyPress=2/ButtonPress=4). `detail` is the keycode
/// or button number; (ex,ey) the event coordinates in the window.
fn send_input(c: &mut XConn, kind: u8, detail: u8, window: u32, ex: i16, ey: i16) {
    let mut e = [0u8; 32];
    e[0] = kind;
    e[1] = detail;
    e[2..4].copy_from_slice(&c.seq.to_le_bytes());
    // time@4=0, root@8=ROOT, event(window)@12, child@16=0
    e[8..12].copy_from_slice(&ROOT_WINDOW.to_le_bytes());
    e[12..16].copy_from_slice(&window.to_le_bytes());
    // root-x@20, root-y@22, event-x@24, event-y@26
    e[24..26].copy_from_slice(&ex.to_le_bytes());
    e[26..28].copy_from_slice(&ey.to_le_bytes());
    e[30] = 1; // same-screen
    c.outbuf.extend_from_slice(&e);
    trace(format_args!("-> input kind={kind} detail={detail} window={window:#x}"));
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
fn fill_rect(c: &mut XConn, draw: u32, rx: i32, ry: i32, rw: i32, rh: i32, argb: u32) {
    if let Some(win) = c.windows.iter_mut().find(|w| w.id == draw) {
        let ww = win.w as i32;
        let wh = win.h as i32;
        for yy in ry.max(0)..(ry + rh).min(wh) {
            for xx in rx.max(0)..(rx + rw).min(ww) {
                win.buf[(yy * ww + xx) as usize] = argb;
            }
        }
    }
}

/// Present the given (mapped) window to the real framebuffer + verify a sample
/// pixel for the bring-up log. Reuses the app-graphics XRGB blit (as DOOM does).
fn present(c: &XConn, id: u32) {
    if let Some(win) = c.windows.iter().find(|w| w.id == id && w.mapped) {
        crate::screen_present_xrgb(&win.buf, win.w as usize, win.h as usize);
        let ctr = (win.h as usize / 2) * win.w as usize + win.w as usize / 2;
        let sample = win.buf.get(ctr).copied().unwrap_or(0);
        trace(format_args!("present id={id:#x} {}x{} centre-pixel={sample:#08x}", win.w, win.h));
    }
}

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
fn setup_reply(swap: bool) -> Vec<u8> {
    let vendor = b"EuroOS X";
    let vlen = vendor.len();
    // Additional data (after the 8-byte header):
    let mut a: Vec<u8> = Vec::new();
    p32(&mut a, swap, 11_000_000); // release-number
    p32(&mut a, swap, RID_BASE); // resource-id-base
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
