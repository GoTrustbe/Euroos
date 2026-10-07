//! # eurordp: an RDP client core, from scratch, `no_std`
//!
//! Sprint N4 (the one legacy Windows application): the protocol half of an RDP
//! client, host-tested. The transport (TCP, then TLS after the X.224 exchange)
//! is the caller's: the kernel drives it with EuroTLS, a host probe with any
//! TLS library. [`Session`] is a state machine: feed it the bytes the server
//! sent (whole TPKT frames), take the bytes to send, and get [`Event`]s out:
//! bitmap rectangles to paint and session state changes. Input goes the other
//! way through [`Session::input`].
//!
//! Scope of v1: RDP 5+ connection sequence with TLS security (encryption level
//! NONE), no licensing exchange (a server that insists on one ends the
//! session), no drawing orders (the server sends bitmaps), uncompressed
//! bitmaps at 16/24/32 bpp, keyboard scancodes + unicode, mouse buttons,
//! wheel and motion, no virtual channels (clipboard is v2).
#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod mcs;
pub mod rdp;
pub mod x224;

use alloc::string::String;
use alloc::vec::Vec;

/// Where the session is in the connection sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// X.224 Connection Request sent, waiting for the Confirm. Still plain TCP.
    X224,
    /// TLS must be started by the caller now; then call [`Session::tls_ready`].
    StartTls,
    /// MCS Connect-Initial sent.
    McsConnect,
    ErectAttach,
    JoinUser,
    JoinIo,
    /// Client Info sent; waiting for licensing.
    Licensing,
    /// Waiting for Demand Active.
    Capabilities,
    /// Finalization: waiting for the server's synchronize/control/font map.
    Finalizing,
    /// Updates flow; input may be sent.
    Active,
    Ended,
}

/// What the caller must act on.
#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    /// Start TLS on the connection (the server selected PROTOCOL_SSL).
    StartTls,
    /// The session is active: `width` x `height` at `bpp`.
    Active { width: u16, height: u16, bpp: u16 },
    /// Paint these pixels (XRGB8888, row-major, `w*h`) at (x, y).
    Bitmap { x: u16, y: u16, w: u16, h: u16, pixels: Vec<u32> },
    /// The server ended the session or something went wrong.
    Ended(String),
}

pub struct Session {
    phase: Phase,
    out: Vec<u8>,
    events: Vec<Event>,
    inbuf: Vec<u8>,
    core: mcs::ClientCore,
    user: String,
    password: String,
    domain: String,
    server: mcs::ServerData,
    user_id: u16,
    share_id: u32,
    server_user: u16,
    width: u16,
    height: u16,
    bpp: u16,
    final_seen: u8,
    time_ms: u32,
}

impl Session {
    /// Start a session: queues the X.224 Connection Request (asking for TLS).
    pub fn connect(width: u16, height: u16, domain: &str, user: &str, password: &str) -> Session {
        let mut name = [0u8; 16];
        name[..6].copy_from_slice(b"EUROOS");
        let mut s = Session {
            phase: Phase::X224,
            out: Vec::new(),
            events: Vec::new(),
            inbuf: Vec::new(),
            core: mcs::ClientCore { width, height, name, keyboard_layout: 0x409, selected_protocol: x224::PROTOCOL_SSL },
            user: String::from(user),
            password: String::from(password),
            domain: String::from(domain),
            server: mcs::ServerData::default(),
            user_id: 0,
            share_id: 0,
            server_user: 1002,
            width,
            height,
            bpp: 16,
            final_seen: 0,
            time_ms: 0,
        };
        s.out = x224::connection_request(x224::PROTOCOL_SSL, Some("euroos"));
        s
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Bytes to send to the server now (drained).
    pub fn take_output(&mut self) -> Vec<u8> {
        core::mem::take(&mut self.out)
    }

    /// Events for the caller (drained).
    pub fn take_events(&mut self) -> Vec<Event> {
        core::mem::take(&mut self.events)
    }

    /// The caller finished the TLS handshake: continue with MCS.
    pub fn tls_ready(&mut self) {
        if self.phase == Phase::StartTls {
            self.out.extend(x224::data(&mcs::connect_initial(&self.core)));
            self.phase = Phase::McsConnect;
        }
    }

    /// Feed bytes received from the server (any chunking); whole TPKT frames are
    /// processed as they complete.
    pub fn receive(&mut self, bytes: &[u8]) {
        self.inbuf.extend_from_slice(bytes);
        loop {
            let Some(len) = x224::tpkt_len(&self.inbuf) else {
                if self.inbuf.len() >= 4 {
                    self.end("not a TPKT stream");
                }
                return;
            };
            if len < 4 || self.inbuf.len() < len {
                return;
            }
            let frame: Vec<u8> = self.inbuf.drain(..len).collect();
            self.frame(&frame[4..]);
            if self.phase == Phase::Ended {
                return;
            }
        }
    }

    fn end(&mut self, why: &str) {
        if self.phase != Phase::Ended {
            self.phase = Phase::Ended;
            self.events.push(Event::Ended(String::from(why)));
        }
    }

    fn send_mcs(&mut self, data: &[u8]) {
        let io = self.server.io_channel;
        let pdu = mcs::send_data_request(self.user_id, io, data);
        self.out.extend(x224::data(&pdu));
    }

    fn frame(&mut self, tpdu: &[u8]) {
        match self.phase {
            Phase::X224 => match x224::parse_connection_confirm(tpdu) {
                Ok(p) if p & x224::PROTOCOL_SSL != 0 => {
                    self.phase = Phase::StartTls;
                    self.events.push(Event::StartTls);
                }
                Ok(_) => self.end("server refused TLS (standard RDP security is not supported)"),
                Err(e) => self.end(match e {
                    x224::NegFailure::HybridRequiredByServer => "server requires CredSSP/NLA (not supported in v1)",
                    _ => "X.224 negotiation failed",
                }),
            },
            Phase::StartTls => {}
            _ => {
                let Some(p) = x224::data_payload(tpdu) else { return };
                let p: Vec<u8> = p.to_vec();
                self.mcs_pdu(&p);
            }
        }
    }

    fn mcs_pdu(&mut self, p: &[u8]) {
        if mcs::is_disconnect(p) {
            self.end("server disconnected");
            return;
        }
        match self.phase {
            Phase::McsConnect => match mcs::parse_connect_response(p) {
                Some(sd) => {
                    if sd.encryption_level != 0 {
                        self.end("server wants RDP-level encryption (not supported: use TLS)");
                        return;
                    }
                    self.server = sd;
                    self.out.extend(x224::data(&mcs::erect_domain_request()));
                    self.out.extend(x224::data(&mcs::attach_user_request()));
                    self.phase = Phase::ErectAttach;
                }
                None => self.end("bad MCS Connect-Response"),
            },
            Phase::ErectAttach => match mcs::parse_attach_user_confirm(p) {
                Some(uid) => {
                    self.user_id = uid;
                    self.out.extend(x224::data(&mcs::channel_join_request(uid, uid)));
                    self.phase = Phase::JoinUser;
                }
                None => self.end("attach user failed"),
            },
            Phase::JoinUser => match mcs::parse_channel_join_confirm(p) {
                Some(_) => {
                    let io = self.server.io_channel;
                    self.out.extend(x224::data(&mcs::channel_join_request(self.user_id, io)));
                    self.phase = Phase::JoinIo;
                }
                None => self.end("channel join (user) failed"),
            },
            Phase::JoinIo => match mcs::parse_channel_join_confirm(p) {
                Some(_) => {
                    let info = rdp::client_info(&self.domain, &self.user, &self.password);
                    self.send_mcs(&info);
                    self.phase = Phase::Licensing;
                }
                None => self.end("channel join (io) failed"),
            },
            Phase::Licensing | Phase::Capabilities | Phase::Finalizing | Phase::Active => {
                let Some((_, data)) = mcs::parse_send_data_indication(p) else { return };
                let data: Vec<u8> = data.to_vec();
                self.io_pdu(&data);
            }
            _ => {}
        }
    }

    fn io_pdu(&mut self, d: &[u8]) {
        if self.phase == Phase::Licensing {
            match rdp::parse_license(d) {
                Some(rdp::License::Done) => {
                    self.phase = Phase::Capabilities;
                    return;
                }
                Some(rdp::License::Request(t)) => {
                    let _ = t;
                    self.end("server requires a licensing exchange (not supported in v1)");
                    return;
                }
                None => {
                    // Some servers skip licensing and go straight to Demand Active.
                    self.phase = Phase::Capabilities;
                }
            }
        }
        let Some((_, typ, body)) = rdp::share_control(d) else { return };
        match typ {
            rdp::PDUTYPE_DEMANDACTIVE => {
                let Some(da) = rdp::parse_demand_active(body) else { self.end("bad Demand Active"); return };
                self.share_id = da.share_id;
                if da.width != 0 && da.height != 0 {
                    self.width = da.width;
                    self.height = da.height;
                }
                self.bpp = 16;
                let ca = rdp::confirm_active(self.share_id, self.width, self.height, self.user_id);
                self.send_mcs(&ca);
                let s = rdp::synchronize(self.share_id, self.user_id, self.server_user);
                self.send_mcs(&s);
                let c1 = rdp::control(self.share_id, self.user_id, 4);
                self.send_mcs(&c1);
                let c2 = rdp::control(self.share_id, self.user_id, 1);
                self.send_mcs(&c2);
                let f = rdp::font_list(self.share_id, self.user_id);
                self.send_mcs(&f);
                self.final_seen = 0;
                self.phase = Phase::Finalizing;
            }
            rdp::PDUTYPE_DEACTIVATEALL => {
                self.phase = Phase::Capabilities; // a new Demand Active follows
            }
            rdp::PDUTYPE_DATA => match rdp::parse_data(body) {
                Some(rdp::ServerData::Bitmaps(rects)) => {
                    let mut evs = Vec::new();
                    for r in rects {
                        if r.compressed {
                            continue; // v1 negotiates uncompressed bitmaps; ignore others
                        }
                        if let Some(px) = bitmap_to_xrgb(&r) {
                            evs.push(Event::Bitmap { x: r.left, y: r.top, w: r.width, h: r.height, pixels: px });
                        }
                    }
                    self.events.extend(evs);
                }
                Some(rdp::ServerData::Synchronize) | Some(rdp::ServerData::Control(_)) | Some(rdp::ServerData::FontMap) => {
                    if self.phase == Phase::Finalizing {
                        self.final_seen += 1;
                        if self.final_seen >= 4 || matches!(rdp::parse_data(body), Some(rdp::ServerData::FontMap)) {
                            self.phase = Phase::Active;
                            self.events.push(Event::Active { width: self.width, height: self.height, bpp: self.bpp });
                        }
                    }
                }
                Some(rdp::ServerData::ErrorInfo(code)) => {
                    if code != 0 {
                        self.end("server error info");
                    }
                }
                _ => {}
            },
            rdp::PDUTYPE_SERVER_REDIR => self.end("server redirection (not supported)"),
            _ => {}
        }
    }

    /// Queue input events (only while active).
    pub fn input(&mut self, events: &[rdp::Input]) {
        if self.phase != Phase::Active || events.is_empty() {
            return;
        }
        self.time_ms = self.time_ms.wrapping_add(16);
        let pdu = rdp::input_pdu(self.share_id, self.user_id, events, self.time_ms);
        self.send_mcs(&pdu);
    }
}

/// Convert one uncompressed bitmap rectangle (bottom-up rows, as RDP sends
/// them) to top-down XRGB8888. 15/16/24/32 bpp.
pub fn bitmap_to_xrgb(r: &rdp::BitmapRect) -> Option<Vec<u32>> {
    let (w, h) = (r.width as usize, r.height as usize);
    if w == 0 || h == 0 || w > 4096 || h > 4096 {
        return None;
    }
    let bytes_pp = match r.bpp { 15 | 16 => 2, 24 => 3, 32 => 4, _ => return None };
    let stride = (w * bytes_pp + 3) & !3; // rows padded to 4 bytes
    if r.data.len() < stride * h {
        return None;
    }
    let mut out = Vec::with_capacity(w * h);
    for row in 0..h {
        let src = &r.data[(h - 1 - row) * stride..];
        for x in 0..w {
            let px = match r.bpp {
                16 => {
                    let v = u16::from_le_bytes([src[x * 2], src[x * 2 + 1]]);
                    let rr = ((v >> 11) & 0x1f) as u32;
                    let g = ((v >> 5) & 0x3f) as u32;
                    let b = (v & 0x1f) as u32;
                    ((rr << 3 | rr >> 2) << 16) | ((g << 2 | g >> 4) << 8) | (b << 3 | b >> 2)
                }
                15 => {
                    let v = u16::from_le_bytes([src[x * 2], src[x * 2 + 1]]);
                    let rr = ((v >> 10) & 0x1f) as u32;
                    let g = ((v >> 5) & 0x1f) as u32;
                    let b = (v & 0x1f) as u32;
                    ((rr << 3 | rr >> 2) << 16) | ((g << 3 | g >> 2) << 8) | (b << 3 | b >> 2)
                }
                24 => (src[x * 3 + 2] as u32) << 16 | (src[x * 3 + 1] as u32) << 8 | src[x * 3] as u32,
                _ => (src[x * 4 + 2] as u32) << 16 | (src[x * 4 + 1] as u32) << 8 | src[x * 4] as u32,
            };
            out.push(px);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An MCS Send Data Indication from the server (what the session parses).
    fn sdi(channel: u16, data: &[u8]) -> Vec<u8> {
        let mut v = alloc::vec![0x68, 0x00, 0x01];
        v.extend_from_slice(&channel.to_be_bytes());
        v.push(0x70);
        if data.len() < 0x80 { v.push(data.len() as u8) } else { v.extend_from_slice(&[0x80 | (data.len() >> 8) as u8, data.len() as u8]) }
        v.extend_from_slice(data);
        v
    }

    #[test]
    fn session_walks_the_connection_sequence() {
        let mut s = Session::connect(800, 600, "", "euro", "pw");
        let cr = s.take_output();
        assert_eq!(cr[5], 0xE0);
        // Connection Confirm with TLS
        s.receive(&x224::tpkt(&[14, 0xD0, 0, 0, 0x12, 0x34, 0, 2, 0, 8, 0, 1, 0, 0, 0]));
        assert_eq!(s.phase(), Phase::StartTls);
        assert_eq!(s.take_events(), alloc::vec![Event::StartTls]);
        s.tls_ready();
        assert_eq!(s.phase(), Phase::McsConnect);
        let ci = s.take_output();
        assert_eq!(&ci[7..9], &[0x7F, 0x65]);
        // Connect-Response
        let mut r = alloc::vec![0x7F, 0x66, 0x5A, 0x0A, 0x01, 0x00];
        r.extend_from_slice(b"McDn");
        let mut blocks = Vec::new();
        blocks.extend_from_slice(&[0x01, 0x0C, 0x08, 0x00, 0x04, 0x00, 0x08, 0x00]);
        blocks.extend_from_slice(&[0x02, 0x0C, 0x0C, 0x00, 0, 0, 0, 0, 0, 0, 0, 0]);
        blocks.extend_from_slice(&[0x03, 0x0C, 0x08, 0x00, 0xEB, 0x03, 0x00, 0x00]);
        r.push(blocks.len() as u8);
        r.extend_from_slice(&blocks);
        s.receive(&x224::data(&r));
        assert_eq!(s.phase(), Phase::ErectAttach);
        let o = s.take_output();
        assert!(o.windows(5).any(|w| w == [0x04, 1, 0, 1, 0]));
        s.receive(&x224::data(&[0x2E, 0x00, 0x00, 0x06]));
        assert_eq!(s.phase(), Phase::JoinUser);
        s.receive(&x224::data(&[0x3E, 0, 0, 6, 0x03, 0xEF, 0x03, 0xEF]));
        assert_eq!(s.phase(), Phase::JoinIo);
        s.receive(&x224::data(&[0x3E, 0, 0, 6, 0x03, 0xEB, 0x03, 0xEB]));
        assert_eq!(s.phase(), Phase::Licensing);
        let info = s.take_output();
        assert!(info.windows(2).any(|w| w == [0x40, 0x00])); // SEC_INFO_PKT inside
        // licensing: valid client
        let lic = [0x80u8, 0, 0, 0, 0xFF, 3, 0x14, 0, 1, 2, 3, 4];
        s.receive(&x224::data(&sdi(1003, &lic)));
        assert_eq!(s.phase(), Phase::Capabilities);
        // demand active 1024x768 @ 24
        let mut body = Vec::new();
        body.extend_from_slice(&0x10002u32.to_le_bytes());
        body.extend_from_slice(&4u16.to_le_bytes());
        body.extend_from_slice(&(4 + 4 + 28u16).to_le_bytes());
        body.extend_from_slice(b"RDP\0");
        body.extend_from_slice(&[1, 0, 0, 0]);
        let mut bm = alloc::vec![24u8, 0, 1, 0, 1, 0, 1, 0];
        bm.extend_from_slice(&1024u16.to_le_bytes());
        bm.extend_from_slice(&768u16.to_le_bytes());
        bm.extend_from_slice(&[0u8; 12]);
        body.extend_from_slice(&2u16.to_le_bytes());
        body.extend_from_slice(&((bm.len() + 4) as u16).to_le_bytes());
        body.extend_from_slice(&bm);
        let mut da = Vec::new();
        da.extend_from_slice(&((body.len() + 6) as u16).to_le_bytes());
        da.extend_from_slice(&(1u16 | 0x10).to_le_bytes());
        da.extend_from_slice(&1002u16.to_le_bytes());
        da.extend_from_slice(&body);
        s.receive(&x224::data(&sdi(1003, &da)));
        assert_eq!(s.phase(), Phase::Finalizing);
        let fin = s.take_output();
        assert!(fin.len() > 300); // confirm active + 4 finalization PDUs
        // server: font map ends finalization
        let mut fm = alloc::vec![0u8; 12];
        fm[8] = rdp::PDUTYPE2_FONTMAP;
        let mut fmp = Vec::new();
        fmp.extend_from_slice(&((fm.len() + 6) as u16).to_le_bytes());
        fmp.extend_from_slice(&(7u16 | 0x10).to_le_bytes());
        fmp.extend_from_slice(&1002u16.to_le_bytes());
        fmp.extend_from_slice(&fm);
        s.receive(&x224::data(&sdi(1003, &fmp)));
        assert_eq!(s.phase(), Phase::Active);
        assert_eq!(s.take_events(), alloc::vec![Event::Active { width: 1024, height: 768, bpp: 16 }]);
        s.input(&[rdp::Input::Move { x: 1, y: 2 }]);
        assert!(!s.take_output().is_empty());
    }

    #[test]
    fn bitmap_conversion_is_bottom_up_and_565() {
        let data = [0xFFu8, 0xFF, 0x00, 0x00, 0xE0, 0x07, 0x1F, 0x00]; // row0 (bottom): white, black; row1 (top): green, blue
        let r = rdp::BitmapRect { left: 0, top: 0, right: 1, bottom: 1, width: 2, height: 2, bpp: 16, compressed: false, data: &data };
        let px = bitmap_to_xrgb(&r).unwrap();
        assert_eq!(px, alloc::vec![0x00FF00, 0x0000FF, 0xFFFFFF, 0x000000]);
    }
}
