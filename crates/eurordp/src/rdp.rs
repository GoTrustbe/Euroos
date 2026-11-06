//! The RDP connection sequence after MCS (MS-RDPBCGR 1.3.1.1): Client Info,
//! licensing, Demand Active / Confirm Active with capability sets, the
//! finalization PDUs (synchronize, control, font list) and then the session:
//! slow-path update PDUs (bitmap updates) in, input event PDUs out.
//!
//! Every PDU here is the payload of an MCS Send Data Request/Indication on the
//! I/O channel. With TLS and encryption level NONE there is no RDP security
//! header on data PDUs; only the Client Info and licensing PDUs carry the
//! 4-byte basic header (flags + flagsHi).

use alloc::vec::Vec;

pub const SEC_INFO_PKT: u16 = 0x0040;
pub const SEC_LICENSE_PKT: u16 = 0x0080;

// Share control PDU types
pub const PDUTYPE_DEMANDACTIVE: u16 = 1;
pub const PDUTYPE_CONFIRMACTIVE: u16 = 3;
pub const PDUTYPE_DEACTIVATEALL: u16 = 6;
pub const PDUTYPE_DATA: u16 = 7;
pub const PDUTYPE_SERVER_REDIR: u16 = 10;
// Share data PDU types (pduType2)
pub const PDUTYPE2_UPDATE: u8 = 2;
pub const PDUTYPE2_CONTROL: u8 = 20;
pub const PDUTYPE2_POINTER: u8 = 27;
pub const PDUTYPE2_INPUT: u8 = 28;
pub const PDUTYPE2_SYNCHRONIZE: u8 = 31;
pub const PDUTYPE2_FONTLIST: u8 = 39;
pub const PDUTYPE2_FONTMAP: u8 = 40;
pub const PDUTYPE2_SET_ERROR_INFO: u8 = 47;

fn u16le(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}
fn u32le(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

fn utf16(s: &str, v: &mut Vec<u8>) -> u16 {
    let start = v.len();
    for u in s.encode_utf16() {
        v.extend_from_slice(&u.to_le_bytes());
    }
    let n = (v.len() - start) as u16;
    v.extend_from_slice(&[0, 0]); // terminator
    n
}

/// Client Info PDU (2.2.1.11): credentials and session options, with the basic
/// security header (SEC_INFO_PKT). `password` may be empty for a logon prompt.
pub fn client_info(domain: &str, user: &str, password: &str) -> Vec<u8> {
    let mut strings = Vec::new();
    let cb_domain = utf16(domain, &mut strings);
    let cb_user = utf16(user, &mut strings);
    let cb_pass = utf16(password, &mut strings);
    let cb_shell = utf16("", &mut strings);
    let cb_dir = utf16("", &mut strings);
    let mut v = Vec::new();
    v.extend_from_slice(&SEC_INFO_PKT.to_le_bytes());
    v.extend_from_slice(&0u16.to_le_bytes()); // flagsHi
    v.extend_from_slice(&0u32.to_le_bytes()); // codePage
    // INFO_MOUSE | INFO_DISABLECTRLALTDEL | INFO_UNICODE | INFO_MAXIMIZESHELL | INFO_ENABLEWINDOWSKEY | INFO_AUTOLOGON(if password)
    let mut flags: u32 = 0x0001 | 0x0002 | 0x0010 | 0x0020 | 0x0100;
    if !password.is_empty() {
        flags |= 0x0008;
    }
    v.extend_from_slice(&flags.to_le_bytes());
    for cb in [cb_domain, cb_user, cb_pass, cb_shell, cb_dir] {
        v.extend_from_slice(&cb.to_le_bytes());
    }
    v.extend_from_slice(&strings);
    // TS_EXTENDED_INFO_PACKET: AF_INET, empty address and dir, zero time zone,
    // session id 0, performance flags (no wallpaper, no animations, no themes:
    // fewer bitmap updates), no auto-reconnect cookie.
    v.extend_from_slice(&2u16.to_le_bytes()); // clientAddressFamily AF_INET
    v.extend_from_slice(&2u16.to_le_bytes()); // cbClientAddress (incl. terminator)
    v.extend_from_slice(&[0, 0]);
    v.extend_from_slice(&2u16.to_le_bytes()); // cbClientDir
    v.extend_from_slice(&[0, 0]);
    v.extend_from_slice(&[0u8; 172]); // clientTimeZone
    v.extend_from_slice(&0u32.to_le_bytes()); // clientSessionId
    v.extend_from_slice(&(0x0001u32 | 0x0004 | 0x0008).to_le_bytes()); // performanceFlags
    v.extend_from_slice(&0u16.to_le_bytes()); // cbAutoReconnectLen
    v
}

/// What a licensing PDU (SEC_LICENSE_PKT) tells us.
#[derive(Debug, PartialEq, Eq)]
pub enum License {
    /// STATUS_VALID_CLIENT (or any error alert): licensing is finished.
    Done,
    /// The server wants a licensing exchange we do not implement (v1).
    Request(u8),
}

/// Parse a PDU with a basic security header; Some(License) when it is a
/// licensing PDU, None when it is not.
pub fn parse_license(pdu: &[u8]) -> Option<License> {
    if pdu.len() < 8 {
        return None;
    }
    let flags = u16le(pdu, 0);
    if flags & SEC_LICENSE_PKT == 0 {
        return None;
    }
    let msg = pdu[4];
    Some(if msg == 0xFF { License::Done } else { License::Request(msg) })
}

/// Server Demand Active (2.2.1.13.1): the share id and the server's bitmap
/// capability (preferred depth, desktop size).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DemandActive {
    pub share_id: u32,
    pub server_bpp: u16,
    pub width: u16,
    pub height: u16,
}

/// Share control header: (totalLength, pduType, pduSource) + the body offset.
pub fn share_control(pdu: &[u8]) -> Option<(u16, u16, &[u8])> {
    if pdu.len() < 6 {
        return None;
    }
    let total = u16le(pdu, 0) as usize;
    let typ = u16le(pdu, 2) & 0x0F;
    let end = total.min(pdu.len());
    Some((total as u16, typ, &pdu[6..end]))
}

pub fn parse_demand_active(body: &[u8]) -> Option<DemandActive> {
    if body.len() < 8 {
        return None;
    }
    let share_id = u32le(body, 0);
    let len_src = u16le(body, 4) as usize;
    let len_caps = u16le(body, 6) as usize;
    let mut i = 8 + len_src;
    if i + 4 > body.len() {
        return None;
    }
    let n = u16le(body, i) as usize;
    i += 4; // numberCapabilities + pad
    let mut da = DemandActive { share_id, ..Default::default() };
    let end = (8 + len_src + len_caps).min(body.len());
    for _ in 0..n {
        if i + 4 > end {
            break;
        }
        let typ = u16le(body, i);
        let len = u16le(body, i + 2) as usize;
        if len < 4 || i + len > end {
            break;
        }
        let c = &body[i + 4..i + len];
        if typ == 2 && c.len() >= 8 {
            da.server_bpp = u16le(c, 0);
            da.width = u16le(c, 8); // desktopWidth
            da.height = u16le(c, 10);
        }
        i += len;
    }
    Some(da)
}

fn cap(v: &mut Vec<u8>, typ: u16, body: &[u8]) {
    v.extend_from_slice(&typ.to_le_bytes());
    v.extend_from_slice(&((body.len() + 4) as u16).to_le_bytes());
    v.extend_from_slice(body);
}

/// Client Confirm Active (2.2.1.13.2) with the capability sets a plain
/// bitmap-update client needs: no drawing orders (the server sends bitmaps),
/// uncompressed bitmaps at 16 bpp, scancode + unicode keyboard, mouse.
pub fn confirm_active(share_id: u32, width: u16, height: u16, user_id: u16) -> Vec<u8> {
    let mut caps = Vec::new();
    let mut n = 0u16;
    // General (0x01): osMajorType 1 (Windows), minor 3 (NT), protocolVersion 0x200,
    // extraFlags 0 (no fast-path output: slow-path updates only), refresh/no-bitmap-compression
    let mut g = Vec::new();
    g.extend_from_slice(&1u16.to_le_bytes());
    g.extend_from_slice(&3u16.to_le_bytes());
    g.extend_from_slice(&0x0200u16.to_le_bytes());
    g.extend_from_slice(&0u16.to_le_bytes()); // pad
    g.extend_from_slice(&0u16.to_le_bytes()); // compression types
    g.extend_from_slice(&0u16.to_le_bytes()); // extraFlags
    g.extend_from_slice(&0u16.to_le_bytes()); // updateCapabilityFlag
    g.extend_from_slice(&0u16.to_le_bytes()); // remoteUnshareFlag
    g.extend_from_slice(&0u16.to_le_bytes()); // compressionLevel
    g.push(0); // refreshRectSupport
    g.push(0); // suppressOutputSupport
    cap(&mut caps, 0x01, &g); n += 1;
    // Bitmap (0x02): 16 bpp, desktop size, no compression, multiple rectangles
    let mut b = Vec::new();
    b.extend_from_slice(&16u16.to_le_bytes()); // preferredBitsPerPixel
    b.extend_from_slice(&1u16.to_le_bytes()); // receive1BitPerPixel
    b.extend_from_slice(&1u16.to_le_bytes()); // receive4
    b.extend_from_slice(&1u16.to_le_bytes()); // receive8
    b.extend_from_slice(&width.to_le_bytes());
    b.extend_from_slice(&height.to_le_bytes());
    b.extend_from_slice(&0u16.to_le_bytes()); // pad
    b.extend_from_slice(&1u16.to_le_bytes()); // desktopResizeFlag
    b.extend_from_slice(&0u16.to_le_bytes()); // bitmapCompressionFlag: 0 = uncompressed
    b.push(0); // highColorFlags
    b.push(0); // drawingFlags
    b.extend_from_slice(&1u16.to_le_bytes()); // multipleRectangleSupport
    b.extend_from_slice(&0u16.to_le_bytes()); // pad
    cap(&mut caps, 0x02, &b); n += 1;
    // Order (0x03): no orders at all, so every update is a bitmap
    let mut o = Vec::new();
    o.extend_from_slice(&[0u8; 16]); // terminalDescriptor
    o.extend_from_slice(&0u32.to_le_bytes()); // pad
    o.extend_from_slice(&1u16.to_le_bytes()); // desktopSaveXGranularity
    o.extend_from_slice(&20u16.to_le_bytes()); // desktopSaveYGranularity
    o.extend_from_slice(&0u16.to_le_bytes()); // pad
    o.extend_from_slice(&1u16.to_le_bytes()); // maximumOrderLevel
    o.extend_from_slice(&0u16.to_le_bytes()); // numberFonts
    o.extend_from_slice(&(0x0002u16 | 0x0008).to_le_bytes()); // orderFlags: NEGOTIATE | ZEROBOUNDSDELTAS
    o.extend_from_slice(&[0u8; 32]); // orderSupport: none
    o.extend_from_slice(&0u16.to_le_bytes()); // textFlags
    o.extend_from_slice(&0u16.to_le_bytes()); // orderSupportExFlags
    o.extend_from_slice(&0u32.to_le_bytes()); // pad
    o.extend_from_slice(&0u32.to_le_bytes()); // desktopSaveSize
    o.extend_from_slice(&0u16.to_le_bytes()); // pad
    o.extend_from_slice(&0u16.to_le_bytes()); // pad
    o.extend_from_slice(&0xE4u16.to_le_bytes()); // textANSICodePage
    o.extend_from_slice(&0u16.to_le_bytes()); // pad
    cap(&mut caps, 0x03, &o); n += 1;
    // Bitmap cache (0x04): 40 bytes, all zero (no cache)
    cap(&mut caps, 0x04, &[0u8; 36]); n += 1;
    // Pointer (0x08): colour pointers, cache 20
    let mut p = Vec::new();
    p.extend_from_slice(&1u16.to_le_bytes());
    p.extend_from_slice(&20u16.to_le_bytes());
    p.extend_from_slice(&20u16.to_le_bytes());
    cap(&mut caps, 0x08, &p); n += 1;
    // Input (0x0D): scancodes + unicode, keyboard layout, IBM enhanced
    let mut inp = Vec::new();
    inp.extend_from_slice(&(0x0001u16 | 0x0010).to_le_bytes()); // INPUT_FLAG_SCANCODES | UNICODE
    inp.extend_from_slice(&0u16.to_le_bytes());
    inp.extend_from_slice(&0x409u32.to_le_bytes());
    inp.extend_from_slice(&4u32.to_le_bytes());
    inp.extend_from_slice(&0u32.to_le_bytes());
    inp.extend_from_slice(&12u32.to_le_bytes());
    inp.extend_from_slice(&[0u8; 64]);
    cap(&mut caps, 0x0D, &inp); n += 1;
    // Share (0x09): nodeId = our user id
    let mut sh = Vec::new();
    sh.extend_from_slice(&user_id.to_le_bytes());
    sh.extend_from_slice(&0u16.to_le_bytes());
    cap(&mut caps, 0x09, &sh); n += 1;
    // Colour cache (0x0A), control (0x05), window activation (0x07), font (0x0E),
    // brush (0x0F), glyph cache (0x10), offscreen (0x11), sound (0x0C),
    // virtual channel (0x14), multifragment update (0x1A)
    cap(&mut caps, 0x0A, &[6, 0, 0, 0]); n += 1;
    cap(&mut caps, 0x05, &[0, 0, 0, 0, 2, 0, 2, 0]); n += 1;
    cap(&mut caps, 0x07, &[0, 0, 0, 0, 0, 0, 0, 0]); n += 1;
    cap(&mut caps, 0x0E, &[1, 0, 0, 0]); n += 1;
    cap(&mut caps, 0x0F, &[0, 0, 0, 0]); n += 1;
    let mut gl = Vec::new();
    for _ in 0..10 { gl.extend_from_slice(&[254, 0, 4, 0]); } // 10 glyph caches: 254 entries of 4 bytes
    gl.extend_from_slice(&[0, 0, 0, 0]); // fragCache
    gl.extend_from_slice(&0u16.to_le_bytes()); // glyphSupportLevel NONE
    gl.extend_from_slice(&0u16.to_le_bytes());
    cap(&mut caps, 0x10, &gl); n += 1;
    cap(&mut caps, 0x11, &[0, 0, 0, 0, 0, 0, 0, 0]); n += 1;
    cap(&mut caps, 0x0C, &[0, 0, 0, 0]); n += 1;
    let mut vc = Vec::new();
    vc.extend_from_slice(&0u32.to_le_bytes());
    vc.extend_from_slice(&1600u32.to_le_bytes());
    cap(&mut caps, 0x14, &vc); n += 1;
    cap(&mut caps, 0x1A, &0x0001_0000u32.to_le_bytes()); n += 1;

    let src = b"EUROOS\0";
    let mut body = Vec::new();
    body.extend_from_slice(&share_id.to_le_bytes());
    body.extend_from_slice(&0x03EAu16.to_le_bytes()); // originatorId
    body.extend_from_slice(&(src.len() as u16).to_le_bytes());
    body.extend_from_slice(&((caps.len() + 4) as u16).to_le_bytes());
    body.extend_from_slice(src);
    body.extend_from_slice(&n.to_le_bytes());
    body.extend_from_slice(&0u16.to_le_bytes());
    body.extend_from_slice(&caps);
    share_control_pdu(PDUTYPE_CONFIRMACTIVE, user_id, &body)
}

fn share_control_pdu(typ: u16, source: u16, body: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(body.len() + 6);
    v.extend_from_slice(&((body.len() + 6) as u16).to_le_bytes());
    v.extend_from_slice(&(typ | 0x10).to_le_bytes());
    v.extend_from_slice(&source.to_le_bytes());
    v.extend_from_slice(body);
    v
}

/// A share data PDU (share control + share data headers around `body`).
pub fn data_pdu(share_id: u32, user_id: u16, typ2: u8, body: &[u8]) -> Vec<u8> {
    let mut d = Vec::with_capacity(body.len() + 12);
    d.extend_from_slice(&share_id.to_le_bytes());
    d.push(0); // pad
    d.push(1); // streamId low
    d.extend_from_slice(&((body.len() + 4) as u16).to_le_bytes()); // uncompressedLength
    d.push(typ2);
    d.push(0); // compressedType
    d.extend_from_slice(&0u16.to_le_bytes()); // compressedLength
    d.extend_from_slice(body);
    share_control_pdu(PDUTYPE_DATA, user_id, &d)
}

pub fn synchronize(share_id: u32, user_id: u16, target: u16) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&1u16.to_le_bytes()); // SYNCMSGTYPE_SYNC
    b.extend_from_slice(&target.to_le_bytes());
    data_pdu(share_id, user_id, PDUTYPE2_SYNCHRONIZE, &b)
}

/// Control PDU: action 4 = cooperate, 1 = request control.
pub fn control(share_id: u32, user_id: u16, action: u16) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&action.to_le_bytes());
    b.extend_from_slice(&0u16.to_le_bytes()); // grantId
    b.extend_from_slice(&0u32.to_le_bytes()); // controlId
    data_pdu(share_id, user_id, PDUTYPE2_CONTROL, &b)
}

pub fn font_list(share_id: u32, user_id: u16) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&0u16.to_le_bytes()); // numberFonts
    b.extend_from_slice(&0u16.to_le_bytes()); // totalNumFonts
    b.extend_from_slice(&3u16.to_le_bytes()); // listFlags FIRST|LAST
    b.extend_from_slice(&50u16.to_le_bytes()); // entrySize
    data_pdu(share_id, user_id, PDUTYPE2_FONTLIST, &b)
}

/// One bitmap rectangle of a slow-path bitmap update (2.2.9.1.1.3.1.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BitmapRect<'a> {
    pub left: u16,
    pub top: u16,
    pub right: u16,
    pub bottom: u16,
    pub width: u16,
    pub height: u16,
    pub bpp: u16,
    pub compressed: bool,
    pub data: &'a [u8],
}

/// What a share data PDU from the server means to the session.
#[derive(Debug, PartialEq, Eq)]
pub enum ServerData<'a> {
    Bitmaps(Vec<BitmapRect<'a>>),
    /// An update we do not render (orders, palette, synchronize).
    OtherUpdate(u16),
    Synchronize,
    Control(u16),
    FontMap,
    Pointer,
    ErrorInfo(u32),
    Other(u8),
}

/// Parse the body of a share data PDU (after the share control header).
pub fn parse_data(body: &[u8]) -> Option<ServerData<'_>> {
    if body.len() < 12 {
        return None;
    }
    let typ2 = body[8];
    let p = &body[12..];
    Some(match typ2 {
        PDUTYPE2_UPDATE => {
            if p.len() < 2 {
                return None;
            }
            let ut = u16le(p, 0);
            if ut != 1 {
                return Some(ServerData::OtherUpdate(ut));
            }
            if p.len() < 4 {
                return None;
            }
            let n = u16le(p, 2) as usize;
            let mut rects = Vec::with_capacity(n);
            let mut i = 4;
            for _ in 0..n {
                if i + 18 > p.len() {
                    break;
                }
                let flags = u16le(p, i + 14);
                let len = u16le(p, i + 16) as usize;
                let mut r = BitmapRect {
                    left: u16le(p, i), top: u16le(p, i + 2), right: u16le(p, i + 4), bottom: u16le(p, i + 6),
                    width: u16le(p, i + 8), height: u16le(p, i + 10), bpp: u16le(p, i + 12),
                    compressed: flags & 0x0001 != 0, data: &[],
                };
                i += 18;
                let mut dlen = len;
                if r.compressed && flags & 0x0400 == 0 {
                    // TS_CD_HEADER (8 bytes) precedes the compressed data
                    if i + 8 > p.len() {
                        break;
                    }
                    dlen = u16le(p, i + 2) as usize + u16le(p, i) as usize; // cbCompFirstRowSize + cbCompMainBodySize
                    i += 8;
                }
                if i + dlen > p.len() {
                    break;
                }
                r.data = &p[i..i + dlen];
                i += dlen;
                rects.push(r);
            }
            ServerData::Bitmaps(rects)
        }
        PDUTYPE2_SYNCHRONIZE => ServerData::Synchronize,
        PDUTYPE2_CONTROL => ServerData::Control(if p.len() >= 2 { u16le(p, 0) } else { 0 }),
        PDUTYPE2_FONTMAP => ServerData::FontMap,
        PDUTYPE2_POINTER => ServerData::Pointer,
        PDUTYPE2_SET_ERROR_INFO => ServerData::ErrorInfo(if p.len() >= 4 { u32le(p, 0) } else { 0 }),
        t => ServerData::Other(t),
    })
}

/// Input events (2.2.8.1.1.3.1.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    /// PC scancode (set 1) down/up; `extended` for the E0-prefixed keys.
    Scancode { code: u8, down: bool, extended: bool },
    Unicode { ch: u16, down: bool },
    Move { x: u16, y: u16 },
    Button { button: u8, down: bool, x: u16, y: u16 },
    Wheel { delta: i16, x: u16, y: u16 },
}

/// Input Event PDU with `events`.
pub fn input_pdu(share_id: u32, user_id: u16, events: &[Input], time_ms: u32) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&(events.len() as u16).to_le_bytes());
    b.extend_from_slice(&0u16.to_le_bytes());
    for ev in events {
        b.extend_from_slice(&time_ms.to_le_bytes());
        match *ev {
            Input::Scancode { code, down, extended } => {
                b.extend_from_slice(&4u16.to_le_bytes());
                let flags: u16 = (if down { 0 } else { 0x8000 }) | (if extended { 0x0100 } else { 0 });
                b.extend_from_slice(&flags.to_le_bytes());
                b.extend_from_slice(&(code as u16).to_le_bytes());
                b.extend_from_slice(&0u16.to_le_bytes());
            }
            Input::Unicode { ch, down } => {
                b.extend_from_slice(&5u16.to_le_bytes());
                b.extend_from_slice(&(if down { 0u16 } else { 0x8000 }).to_le_bytes());
                b.extend_from_slice(&ch.to_le_bytes());
                b.extend_from_slice(&0u16.to_le_bytes());
            }
            Input::Move { x, y } => {
                b.extend_from_slice(&0x8001u16.to_le_bytes());
                b.extend_from_slice(&0x0800u16.to_le_bytes());
                b.extend_from_slice(&x.to_le_bytes());
                b.extend_from_slice(&y.to_le_bytes());
            }
            Input::Button { button, down, x, y } => {
                b.extend_from_slice(&0x8001u16.to_le_bytes());
                let bf: u16 = match button { 1 => 0x1000, 2 => 0x2000, _ => 0x4000 };
                b.extend_from_slice(&(bf | if down { 0x8000 } else { 0 }).to_le_bytes());
                b.extend_from_slice(&x.to_le_bytes());
                b.extend_from_slice(&y.to_le_bytes());
            }
            Input::Wheel { delta, x, y } => {
                b.extend_from_slice(&0x8001u16.to_le_bytes());
                let mag = (delta.unsigned_abs() & 0xFF) as u16;
                let f: u16 = 0x0200 | (if delta < 0 { 0x0100 | ((0x100 - mag) & 0xFF) } else { mag });
                b.extend_from_slice(&f.to_le_bytes());
                b.extend_from_slice(&x.to_le_bytes());
                b.extend_from_slice(&y.to_le_bytes());
            }
        }
    }
    data_pdu(share_id, user_id, PDUTYPE2_INPUT, &b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_info_carries_credentials_and_header() {
        let ci = client_info("", "euro", "pw");
        assert_eq!(u16le(&ci, 0), SEC_INFO_PKT);
        assert_eq!(u16le(&ci, 12 + 2), 8); // cbUserName = 4 UTF-16 chars
        assert_eq!(u16le(&ci, 12 + 4), 4); // cbPassword
        assert!(ci.windows(8).any(|w| w == [b'e', 0, b'u', 0, b'r', 0, b'o', 0]));
        assert_eq!(u32le(&ci, 8) & 0x0008, 0x0008); // autologon with a password
    }

    #[test]
    fn license_pdu_detection() {
        assert_eq!(parse_license(&[0x80, 0, 0, 0, 0xFF, 3, 0x14, 0, 1, 2, 3, 4]), Some(License::Done));
        assert_eq!(parse_license(&[0x80, 0, 0, 0, 0x01, 3, 0x14, 0]), Some(License::Request(1)));
        assert_eq!(parse_license(&[0x40, 0, 0, 0, 0xFF, 3, 0x14, 0]), None);
    }

    #[test]
    fn demand_active_parses_bitmap_caps() {
        let mut body = Vec::new();
        body.extend_from_slice(&0x0001_0002u32.to_le_bytes());
        body.extend_from_slice(&4u16.to_le_bytes()); // lengthSourceDescriptor
        let caps_len: u16 = 4 + 4 + 28;
        body.extend_from_slice(&caps_len.to_le_bytes());
        body.extend_from_slice(b"RDP\0");
        body.extend_from_slice(&1u16.to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes());
        let mut bm = Vec::new();
        bm.extend_from_slice(&24u16.to_le_bytes());
        bm.extend_from_slice(&[1, 0, 1, 0, 1, 0]);
        bm.extend_from_slice(&1280u16.to_le_bytes());
        bm.extend_from_slice(&800u16.to_le_bytes());
        bm.extend_from_slice(&[0u8; 12]);
        cap(&mut body, 2, &bm);
        let da = parse_demand_active(&body).unwrap();
        assert_eq!(da, DemandActive { share_id: 0x10002, server_bpp: 24, width: 1280, height: 800 });
    }

    #[test]
    fn confirm_active_and_finalization_shape() {
        let ca = confirm_active(0x10002, 1024, 768, 1007);
        assert_eq!(u16le(&ca, 0) as usize, ca.len());
        assert_eq!(u16le(&ca, 2), PDUTYPE_CONFIRMACTIVE | 0x10);
        assert_eq!(u16le(&ca, 4), 1007);
        assert_eq!(&ca[16..23], b"EUROOS\0"); // after shareId, originatorId, the two lengths
        let sync = synchronize(0x10002, 1007, 1002);
        assert_eq!(u16le(&sync, 2), PDUTYPE_DATA | 0x10);
        assert_eq!(sync[6 + 8], PDUTYPE2_SYNCHRONIZE);
        assert_eq!(control(1, 1, 4)[6 + 8], PDUTYPE2_CONTROL);
        assert_eq!(font_list(1, 1)[6 + 8], PDUTYPE2_FONTLIST);
    }

    #[test]
    fn bitmap_update_parses_rects() {
        let mut body = alloc::vec![0u8; 12];
        body[8] = PDUTYPE2_UPDATE;
        body.extend_from_slice(&1u16.to_le_bytes()); // UPDATETYPE_BITMAP
        body.extend_from_slice(&1u16.to_le_bytes()); // one rect
        for v in [0u16, 0, 1, 1, 2, 2, 16, 0x0400, 8] { // 2x2 at 16bpp, uncompressed (no hdr flag), 8 bytes
            body.extend_from_slice(&v.to_le_bytes());
        }
        body.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        match parse_data(&body) {
            Some(ServerData::Bitmaps(r)) => {
                assert_eq!(r.len(), 1);
                assert_eq!((r[0].width, r[0].height, r[0].bpp, r[0].compressed), (2, 2, 16, false));
                assert_eq!(r[0].data, &[1, 2, 3, 4, 5, 6, 7, 8]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn input_pdu_encodes_key_and_mouse() {
        let p = input_pdu(7, 1007, &[Input::Scancode { code: 0x1E, down: true, extended: false }, Input::Button { button: 1, down: true, x: 10, y: 20 }], 0);
        let b = &p[6 + 12..];
        assert_eq!(u16le(b, 0), 2);
        assert_eq!(u16le(b, 4 + 4), 4); // scancode message
        assert_eq!(u16le(b, 4 + 8), 0x1E);
        assert_eq!(u16le(b, 4 + 12 + 4), 0x8001); // mouse
        assert_eq!(u16le(b, 4 + 12 + 6), 0x9000); // down | button1
    }
}
