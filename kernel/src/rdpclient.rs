//! # RDP client on the desktop (sprint N4: the one legacy Windows application)
//!
//! The protocol lives in `eurordp` (host-tested); this module is the transport
//! and the window. The desktop loop calls [`step`] once per tick: TCP through
//! the kernel's own stack (`net::TcpConn`), EuroTLS 1.3 after the X.224
//! exchange (the server's certificate is NOT validated in v1: RDP servers use
//! self-signed certificates, the fingerprint is logged; trust-on-first-use
//! comes with the settings UI), then the eurordp session. Every bitmap update
//! lands in a frame that is shown through the Euro API surface of a pseudo
//! process (`RDP_PID`) in the hosted native-app window; keys and pointer
//! events the desktop routes to that window go back as RDP input events.
//!
//! Limits (v1): TLS 1.3 only (EuroTLS), no CredSSP/NLA, licensing with a
//! proprietary server certificate only (xrdp; an X.509 chain is next),
//! uncompressed bitmaps, no clipboard or drive redirection.

use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

use crate::net::TcpConn;
use eurordp::rdp::Input;
use eurordp::{Event, Phase, Session};

pub const RDP_PID: u64 = 0x3_0000;
const IDLE_LIMIT: u32 = 6000; // desktop steps without a byte before giving up (~60 s)

struct Client {
    host: String,
    tcp: TcpConn,
    tls: Option<eurotls::Tls13Client>,
    sess: Session,
    tls_announced: bool,
    frame: Vec<u32>,
    w: u32,
    h: u32,
    idle: u32,
    connected_tcp: bool,
    bitmaps: u64,
}

static CLIENT: Mutex<Option<Client>> = Mutex::new(None);

/// Start a session to `host:port` as `user`. The surface opens at once (grey)
/// and shows the remote desktop as updates arrive.
pub fn start(host: &str, port: u16, user: &str, password: &str, w: u32, h: u32) -> Result<String, String> {
    if CLIENT.lock().is_some() {
        return Err(String::from("rdp: a session is already open (close its window first)"));
    }
    let tcp = crate::net::tcp_connect_nowait_to(host, port).ok_or_else(|| alloc::format!("rdp: cannot resolve or reach {host}"))?;
    let mut sess = Session::connect(w as u16, h as u16, "", user, password);
    {
        // Licensing randomness (client random, pre-master secret, hardware id).
        let mut e = [0u8; 96];
        e[0..32].copy_from_slice(&crate::net::entropy(2u8));
        e[32..64].copy_from_slice(&crate::net::entropy(3u8));
        e[64..96].copy_from_slice(&crate::net::entropy(4u8));
        sess.set_entropy(&e);
    }
    if crate::euroapi::create(RDP_PID, w as usize, h as usize) < 0 {
        return Err(String::from("rdp: surface refused"));
    }
    let frame = alloc::vec![0x2a2a2eu32; (w * h) as usize];
    let _ = crate::euroapi::present(RDP_PID, &frame, w as usize, h as usize);
    *CLIENT.lock() = Some(Client {
        host: String::from(host), tcp, tls: None, sess, tls_announced: false, frame, w, h, idle: 0, connected_tcp: false, bitmaps: 0,
    });
    Ok(alloc::format!("rdp: connecting to {host}:{port} as {user} ({w}x{h}, TLS)"))
}

pub fn active() -> bool {
    CLIENT.lock().is_some()
}

/// Close the session (window closed / Ctrl+Alt+Esc).
pub fn stop() {
    let c = CLIENT.lock().take();
    if let Some(mut c) = c {
        c.tcp.close_nowait();
        crate::euroapi::remove(RDP_PID);
        crate::serial_println!("[rdp] session to {} closed ({} bitmap updates)", c.host, c.bitmaps);
    }
}

fn end(c: &mut Client, why: &str) -> bool {
    crate::serial_println!("[rdp] {}: {why}", c.host);
    c.tcp.close_nowait();
    crate::euroapi::remove(RDP_PID);
    true
}

/// One desktop tick. Returns true when the session ended this tick.
pub fn step() -> bool {
    let mut g = CLIENT.lock();
    let Some(c) = g.as_mut() else { return false };
    // 1. TCP connect
    if !c.connected_tcp {
        match c.tcp.poll_connect() {
            Some(true) => {
                c.connected_tcp = true;
                let out = c.sess.take_output(); // the X.224 Connection Request, in clear
                c.tcp.send_nowait(&out);
            }
            Some(false) => { let _ = end(c, "TCP connection refused"); *g = None; return true; }
            None => {
                c.idle += 1;
                if c.idle % 200 == 0 && !c.tcp.resend_syn() { let _ = end(c, "TCP connect timeout"); *g = None; return true; }
                return false;
            }
        }
    }
    // 2. Receive
    let data = c.tcp.recv_nowait(65536);
    c.tcp.tick();
    if data.is_empty() {
        if !c.tcp.open { let _ = end(c, "connection closed by the server"); *g = None; return true; }
        c.idle += 1;
        if c.idle > IDLE_LIMIT && c.sess.phase() != Phase::Active {
            let _ = end(c, "no answer from the server"); *g = None; return true;
        }
    } else {
        c.idle = 0;
        match c.tls.as_mut() {
            None => c.sess.receive(&data), // still clear: the X.224 Confirm
            Some(tls) => {
                tls.feed(&data);
                match tls.process() {
                    Ok(out) => { if !out.is_empty() { c.tcp.send_nowait(&out); } }
                    Err(eurotls::TlsError::Alert(_)) => { let _ = end(c, "TLS closed"); *g = None; return true; }
                    Err(eurotls::TlsError::Protocol(why)) => {
                        crate::serial_println!("[rdp] TLS: {why}");
                        let _ = end(c, "TLS handshake failed (TLS 1.3 only in v1)"); *g = None; return true;
                    }
                    Err(_) => { let _ = end(c, "TLS error"); *g = None; return true; }
                }
                if tls.is_connected() {
                    if !c.tls_announced {
                        c.tls_announced = true;
                        c.sess.tls_ready();
                        crate::serial_println!("[rdp] TLS 1.3 up with {} (certificate not validated: self-signed RDP servers)", c.host);
                    }
                    let app = tls.take_app_data();
                    if !app.is_empty() {
                        c.sess.receive(&app);
                    }
                }
            }
        }
    }
    // 3. Events from the session
    for ev in c.sess.take_events() {
        match ev {
            Event::StartTls => {
                let random = crate::net::entropy(0u8);
                let secret = crate::net::entropy(1u8);
                let (mut tls, hello) = eurotls::Tls13Client::new(&c.host, random, secret);
                tls.allow_insecure_no_verification(); // see the module note: self-signed RDP certificates (v1)
                c.tcp.send_nowait(&hello);
                c.tls = Some(tls);
            }
            Event::Active { width, height, bpp } => {
                crate::serial_println!("[rdp] session ACTIVE: {width}x{height} @ {bpp} bpp");
                if (width as u32, height as u32) != (c.w, c.h) && width > 0 && height > 0 {
                    c.w = width as u32;
                    c.h = height as u32;
                    c.frame = alloc::vec![0x2a2a2eu32; (c.w * c.h) as usize];
                }
            }
            Event::Bitmap { x, y, w, h, pixels } => {
                let (fw, fh) = (c.w as usize, c.h as usize);
                for row in 0..h as usize {
                    let fy = y as usize + row;
                    if fy >= fh { break; }
                    for col in 0..w as usize {
                        let fx = x as usize + col;
                        if fx >= fw { break; }
                        c.frame[fy * fw + fx] = pixels[row * w as usize + col];
                    }
                }
                c.bitmaps += 1;
            }
            Event::Trace(line) => crate::serial_println!("[rdp] {line}"),
            Event::Ended(why) => { let _ = end(c, &why); *g = None; return true; }
        }
    }
    // 4. Input from the hosted window
    let mut inputs: Vec<Input> = Vec::new();
    while let Some(e) = crate::euroapi::poll(RDP_PID) {
        match e.kind {
            crate::euroapi::EV_KEY => inputs.push(Input::Scancode { code: e.a as u8, down: e.b != 0, extended: false }),
            crate::euroapi::EV_MOVE => inputs.push(Input::Move { x: e.a as u16, y: e.b as u16 }),
            crate::euroapi::EV_BUTTON => {
                let (x, y) = ((e.c >> 16) as u16, (e.c & 0xffff) as u16);
                inputs.push(Input::Button { button: e.a as u8, down: true, x, y });
                inputs.push(Input::Button { button: e.a as u8, down: false, x, y });
            }
            crate::euroapi::EV_CLOSE => { let _ = end(c, "window closed"); *g = None; return true; }
            _ => {}
        }
    }
    if !inputs.is_empty() {
        c.sess.input(&inputs);
    }
    // 5. Send what the session queued (encrypted once TLS is up)
    let out = c.sess.take_output();
    if !out.is_empty() {
        match c.tls.as_mut() {
            Some(tls) if tls.is_connected() => {
                if let Ok(rec) = tls.encrypt_app(&out) { c.tcp.send_nowait(&rec); }
            }
            Some(_) => {} // not before the handshake: the session only queues after tls_ready()
            None => c.tcp.send_nowait(&out),
        }
    }
    // 6. Present
    if c.bitmaps > 0 {
        let _ = crate::euroapi::present(RDP_PID, &c.frame, c.w as usize, c.h as usize);
    }
    false
}

/// Lines for the `rdp` shell command without arguments.
pub fn status_lines() -> Vec<String> {
    match CLIENT.lock().as_ref() {
        Some(c) => alloc::vec![alloc::format!("rdp: {}: phase {:?}, {}x{}, {} bitmap updates", c.host, c.sess.phase(), c.w, c.h, c.bitmaps)],
        None => alloc::vec![String::from("rdp <host>[:port] <user> [password]   (EuroTLS, no NLA; xrdp or a server that accepts TLS without a licensing exchange)")],
    }
}
