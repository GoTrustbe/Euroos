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
            *s = Some(XConn { inbuf: Vec::new(), outbuf: Vec::new(), state: State::PreSetup, seq: 0, swap: false, windows: Vec::new(), gcs: Vec::new(), pixmaps: Vec::new() });
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
static LAST_MOUSE: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(u32::MAX);
pub fn pump_mouse() {
    // ButtonPressMask(0x4) or PointerMotionMask(0x40) selected by any mapped window?
    let wants = {
        let t = XCONNS.lock();
        t.iter().flatten().any(|c| c.windows.iter().any(|w| w.mapped && w.event_mask & 0x44 != 0))
    };
    if !wants {
        return;
    }
    // Left-button press -> ButtonPress(1).
    if let Some((mx, my)) = crate::mouse::take_press() {
        let mut t = XCONNS.lock();
        for conn in t.iter_mut().flatten() {
            let wid = conn.windows.iter().find(|w| w.mapped && w.event_mask & 0x4 != 0).map(|w| w.id);
            if let Some(wid) = wid {
                send_input(conn, 4, 1, wid, mx as i16, my as i16); // ButtonPress, button 1
            }
        }
    }
    // Cursor moved -> MotionNotify(6) (button field 0).
    let (px, py) = crate::mouse::pos();
    let packed = ((px as u32 & 0xffff) << 16) | (py as u32 & 0xffff);
    if LAST_MOUSE.swap(packed, core::sync::atomic::Ordering::Relaxed) != packed {
        let mut t = XCONNS.lock();
        for conn in t.iter_mut().flatten() {
            let wid = conn.windows.iter().find(|w| w.mapped && w.event_mask & 0x40 != 0).map(|w| w.id);
            if let Some(wid) = wid {
                send_input(conn, 6, 0, wid, px as i16, py as i16); // MotionNotify
            }
        }
    }
}

/// Deliver a click (ButtonPress + ButtonRelease, button 1) to the front mapped window
/// at WINDOW-LOCAL coordinates — used by the desktop to route a click on a hosted X
/// app's framed window to the app (so its GTK button activates). IRQ-safe: task 0 must
/// not hold XCONNS across a preemption while the IF=0 client read/process spins on it.
pub fn deliver_button(lx: i16, ly: i16) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut t = XCONNS.lock();
        for conn in t.iter_mut().flatten() {
            // The GTK toplevel routes events to its client-side child widgets itself, so
            // deliver to the largest mapped window (the toplevel) — no mask bit required.
            if let Some(wid) = conn.windows.iter().filter(|w| w.mapped && w.w > 1 && w.h > 1)
                .max_by_key(|w| w.w as u32 * w.h as u32).map(|w| w.id)
            {
                send_input(conn, 4, 1, wid, lx, ly); // ButtonPress, button 1
                send_input(conn, 5, 1, wid, lx, ly); // ButtonRelease, button 1
                trace(format_args!("deliver_button local=({lx},{ly}) -> win {wid:#x}"));
            }
        }
    });
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
        // Everything else (no reply): acknowledged by consuming the request.
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
    if let Some(win) = c.windows.iter().find(|w| w.id == id && w.mapped) {
        // Windowed mode: the desktop compositor draws the frame + pulls the pixels, so
        // skip the fullscreen blit (it would fight the compositor). RETAIN the pixels so
        // the app shows as a framed window and keeps showing after a boot-run client
        // exits, and flag a repaint.
        if X_WINDOWED.load(core::sync::atomic::Ordering::Relaxed) {
            if win.w > 1 && win.h > 1 {
                *RETAINED_WINDOW.lock() = Some((win.w as usize, win.h as usize, win.buf.clone()));
            }
            X_DIRTY.store(true, core::sync::atomic::Ordering::Relaxed);
        } else {
            crate::screen_present_xrgb(&win.buf, win.w as usize, win.h as usize);
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
