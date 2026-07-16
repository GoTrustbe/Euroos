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

struct XConn {
    inbuf: Vec<u8>,   // accumulated request bytes not yet consumed
    outbuf: Vec<u8>,  // reply/event bytes waiting to be read()
    state: State,
    seq: u16,         // last request's sequence number (server-side counter)
    swap: bool,       // client is big-endian (byte-swap multi-byte fields)
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
            *s = Some(XConn { inbuf: Vec::new(), outbuf: Vec::new(), state: State::PreSetup, seq: 0, swap: false });
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
        // CreateGC(55), MapWindow(8), ChangeWindowAttributes(2), CreateWindow(1),
        // etc. — no reply. Acknowledged by consuming the request. (Real drawing +
        // window mapping into the compositor is the next milestone.)
        _ => {}
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
