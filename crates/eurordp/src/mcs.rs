//! T.125 MCS + T.124 GCC as RDP uses them (MS-RDPBCGR 2.2.1.3 .. 2.2.1.9).
//!
//! Connect-Initial / Connect-Response are BER (outer) with the GCC conference
//! create PDUs in PER inside; the domain PDUs after that (erect domain, attach
//! user, channel join, send data) are PER with a one-byte choice header.

use alloc::vec::Vec;

/// What the client announces about itself (GCC Client Core Data).
#[derive(Clone, Debug)]
pub struct ClientCore {
    pub width: u16,
    pub height: u16,
    pub name: [u8; 16], // ASCII, NUL padded (sent as UTF-16LE)
    pub keyboard_layout: u32,
    pub selected_protocol: u32,
}

/// What the server answered (GCC Server Core / Security / Network Data).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServerData {
    pub version: u32,
    pub encryption_method: u32,
    pub encryption_level: u32,
    pub io_channel: u16,
    pub channel_ids: Vec<u16>,
}

fn ber_len(v: &mut Vec<u8>, n: usize) {
    if n < 0x80 {
        v.push(n as u8);
    } else if n < 0x100 {
        v.extend_from_slice(&[0x81, n as u8]);
    } else {
        v.extend_from_slice(&[0x82, (n >> 8) as u8, n as u8]);
    }
}

fn ber_int(v: &mut Vec<u8>, n: u32) {
    v.push(0x02);
    if n < 0x80 {
        v.extend_from_slice(&[1, n as u8]);
    } else if n < 0x8000 {
        v.extend_from_slice(&[2, (n >> 8) as u8, n as u8]);
    } else {
        v.extend_from_slice(&[3, (n >> 16) as u8, (n >> 8) as u8, n as u8]);
    }
}

fn domain_params(v: &mut Vec<u8>, p: [u32; 8]) {
    let mut inner = Vec::new();
    for x in p {
        ber_int(&mut inner, x);
    }
    v.push(0x30);
    ber_len(v, inner.len());
    v.extend_from_slice(&inner);
}

fn per_len(v: &mut Vec<u8>, n: usize) {
    if n < 0x80 {
        v.push(n as u8);
    } else {
        v.extend_from_slice(&[0x80 | ((n >> 8) as u8 & 0x3f), n as u8]);
    }
}

/// GCC Conference Create Request carrying the client data blocks.
fn gcc_create_request(core: &ClientCore) -> Vec<u8> {
    let mut blocks = Vec::new();
    // CS_CORE (0xC001)
    let mut c = Vec::new();
    c.extend_from_slice(&0x0008_0004u32.to_le_bytes()); // version RDP 5+
    c.extend_from_slice(&core.width.to_le_bytes());
    c.extend_from_slice(&core.height.to_le_bytes());
    c.extend_from_slice(&0xCA01u16.to_le_bytes()); // colorDepth 8bpp (legacy field)
    c.extend_from_slice(&0xAA03u16.to_le_bytes()); // SASSequence
    c.extend_from_slice(&core.keyboard_layout.to_le_bytes());
    c.extend_from_slice(&2600u32.to_le_bytes()); // clientBuild
    for i in 0..16 { // clientName: 32 bytes UTF-16LE
        c.push(core.name[i]);
        c.push(0);
    }
    c.extend_from_slice(&4u32.to_le_bytes()); // keyboardType IBM enhanced
    c.extend_from_slice(&0u32.to_le_bytes()); // keyboardSubType
    c.extend_from_slice(&12u32.to_le_bytes()); // keyboardFunctionKey
    c.extend_from_slice(&[0u8; 64]); // imeFileName
    c.extend_from_slice(&0xCA01u16.to_le_bytes()); // postBeta2ColorDepth
    c.extend_from_slice(&1u16.to_le_bytes()); // clientProductId
    c.extend_from_slice(&0u32.to_le_bytes()); // serialNumber
    c.extend_from_slice(&16u16.to_le_bytes()); // highColorDepth 16 bpp
    c.extend_from_slice(&0x000Fu16.to_le_bytes()); // supportedColorDepths 24/16/15/32
    c.extend_from_slice(&0x0001u16.to_le_bytes()); // earlyCapabilityFlags: ERRINFO PDU
    c.extend_from_slice(&[0u8; 64]); // clientDigProductId
    c.push(0); // connectionType
    c.push(0); // pad
    c.extend_from_slice(&core.selected_protocol.to_le_bytes());
    blocks.extend_from_slice(&0xC001u16.to_le_bytes());
    blocks.extend_from_slice(&((c.len() + 4) as u16).to_le_bytes());
    blocks.extend_from_slice(&c);
    // CS_SECURITY (0xC002): no RDP-level encryption (TLS carries the session)
    blocks.extend_from_slice(&0xC002u16.to_le_bytes());
    blocks.extend_from_slice(&12u16.to_le_bytes());
    blocks.extend_from_slice(&0u32.to_le_bytes());
    blocks.extend_from_slice(&0u32.to_le_bytes());
    // CS_NET (0xC003): no static virtual channels in v1
    blocks.extend_from_slice(&0xC003u16.to_le_bytes());
    blocks.extend_from_slice(&8u16.to_le_bytes());
    blocks.extend_from_slice(&0u32.to_le_bytes());

    let mut pdu = Vec::new();
    pdu.extend_from_slice(&[0x00, 0x05, 0x00, 0x14, 0x7C, 0x00, 0x01]); // T.124 key 0.0.20.124.0.1
    let mut ccr = Vec::new();
    ccr.extend_from_slice(&[0x00, 0x08, 0x00, 0x10, 0x00, 0x01, 0xC0, 0x00, 0x44, 0x75, 0x63, 0x61]); // conferenceCreateRequest, h221 "Duca"
    per_len(&mut ccr, blocks.len());
    ccr.extend_from_slice(&blocks);
    per_len(&mut pdu, ccr.len());
    pdu.extend_from_slice(&ccr);
    pdu
}

/// MCS Connect-Initial (BER) with the GCC Conference Create Request inside.
pub fn connect_initial(core: &ClientCore) -> Vec<u8> {
    let user = gcc_create_request(core);
    let mut body = Vec::new();
    body.extend_from_slice(&[0x04, 0x01, 0x01]); // callingDomainSelector
    body.extend_from_slice(&[0x04, 0x01, 0x01]); // calledDomainSelector
    body.extend_from_slice(&[0x01, 0x01, 0xFF]); // upwardFlag TRUE
    domain_params(&mut body, [34, 2, 0, 1, 0, 1, 0xFFFF, 2]); // target
    domain_params(&mut body, [1, 1, 1, 1, 0, 1, 0x420, 2]); // minimum
    domain_params(&mut body, [0xFFFF, 0xFC17, 0xFFFF, 1, 0, 1, 0xFFFF, 2]); // maximum
    body.push(0x04);
    ber_len(&mut body, user.len());
    body.extend_from_slice(&user);
    let mut v = Vec::new();
    v.extend_from_slice(&[0x7F, 0x65]); // Connect-Initial
    ber_len(&mut v, body.len());
    v.extend_from_slice(&body);
    v
}

/// Parse the MCS Connect-Response: the server data blocks inside the GCC
/// Conference Create Response. Scans for the block tags (the BER/PER framing
/// around them varies between servers; the blocks themselves are fixed).
pub fn parse_connect_response(pdu: &[u8]) -> Option<ServerData> {
    if pdu.len() < 4 || pdu[0] != 0x7F || pdu[1] != 0x66 {
        return None;
    }
    // Server data blocks start right after the h221 key "McDn" of the response.
    let start = pdu.windows(4).position(|w| w == b"McDn")? + 4;
    let mut i = start;
    // One PER length determinant for the user data, then blocks.
    if i < pdu.len() && pdu[i] & 0x80 != 0 { i += 2 } else { i += 1 }
    let mut sd = ServerData::default();
    let mut seen = 0;
    while i + 4 <= pdu.len() {
        let typ = u16::from_le_bytes([pdu[i], pdu[i + 1]]);
        let len = u16::from_le_bytes([pdu[i + 2], pdu[i + 3]]) as usize;
        if len < 4 || i + len > pdu.len() {
            break;
        }
        let b = &pdu[i + 4..i + len];
        match typ {
            0x0C01 if b.len() >= 4 => { sd.version = u32::from_le_bytes([b[0], b[1], b[2], b[3]]); seen += 1; }
            0x0C02 if b.len() >= 8 => {
                sd.encryption_method = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                sd.encryption_level = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
                seen += 1;
            }
            0x0C03 if b.len() >= 4 => {
                sd.io_channel = u16::from_le_bytes([b[0], b[1]]);
                let n = u16::from_le_bytes([b[2], b[3]]) as usize;
                for k in 0..n {
                    if 4 + k * 2 + 2 <= b.len() {
                        sd.channel_ids.push(u16::from_le_bytes([b[4 + k * 2], b[5 + k * 2]]));
                    }
                }
                seen += 1;
            }
            _ => {}
        }
        i += len;
    }
    if seen >= 2 { Some(sd) } else { None }
}

pub fn erect_domain_request() -> Vec<u8> {
    alloc::vec![0x04, 0x01, 0x00, 0x01, 0x00]
}

pub fn attach_user_request() -> Vec<u8> {
    alloc::vec![0x28]
}

/// Attach User Confirm -> the user id (initiator), or None on failure.
pub fn parse_attach_user_confirm(pdu: &[u8]) -> Option<u16> {
    if pdu.len() < 4 || pdu[0] >> 2 != 11 || pdu[1] != 0 {
        return None;
    }
    if pdu[0] & 2 == 0 {
        return None; // no initiator present
    }
    Some(u16::from_be_bytes([pdu[2], pdu[3]]) + 1001)
}

pub fn channel_join_request(user_id: u16, channel: u16) -> Vec<u8> {
    let u = (user_id - 1001).to_be_bytes();
    let c = channel.to_be_bytes();
    alloc::vec![0x38, u[0], u[1], c[0], c[1]]
}

/// Channel Join Confirm -> the joined channel id, or None on failure.
pub fn parse_channel_join_confirm(pdu: &[u8]) -> Option<u16> {
    if pdu.len() < 6 || pdu[0] >> 2 != 15 || pdu[1] != 0 {
        return None;
    }
    Some(u16::from_be_bytes([pdu[4], pdu[5]]))
}

/// Send Data Request: `data` from `user_id` on `channel` (priority high, whole segment).
pub fn send_data_request(user_id: u16, channel: u16, data: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(data.len() + 8);
    v.push(0x64);
    v.extend_from_slice(&(user_id - 1001).to_be_bytes());
    v.extend_from_slice(&channel.to_be_bytes());
    v.push(0x70);
    per_len(&mut v, data.len());
    v.extend_from_slice(data);
    v
}

/// Send Data Indication -> (channel, data).
pub fn parse_send_data_indication(pdu: &[u8]) -> Option<(u16, &[u8])> {
    if pdu.len() < 7 || pdu[0] >> 2 != 26 {
        return None;
    }
    let channel = u16::from_be_bytes([pdu[3], pdu[4]]);
    let mut i = 6;
    let len = if pdu[i] & 0x80 != 0 {
        let l = (((pdu[i] & 0x3f) as usize) << 8) | pdu.get(i + 1).copied()? as usize;
        i += 2;
        l
    } else {
        let l = pdu[i] as usize;
        i += 1;
        l
    };
    if i + len > pdu.len() {
        return None;
    }
    Some((channel, &pdu[i..i + len]))
}

/// Is this a Disconnect Provider Ultimatum (the server ended the session)?
pub fn is_disconnect(pdu: &[u8]) -> bool {
    !pdu.is_empty() && pdu[0] >> 2 == 8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn core() -> ClientCore {
        ClientCore { width: 1024, height: 768, name: *b"EUROOS\0\0\0\0\0\0\0\0\0\0", keyboard_layout: 0x409, selected_protocol: 1 }
    }

    #[test]
    fn connect_initial_is_well_formed_ber() {
        let ci = connect_initial(&core());
        assert_eq!(&ci[..2], &[0x7F, 0x65]);
        assert_eq!(ci[2], 0x82);
        let len = ((ci[3] as usize) << 8) | ci[4] as usize;
        assert_eq!(len, ci.len() - 5);
        // the T.124 key and "Duca" are inside
        assert!(ci.windows(7).any(|w| w == [0x00, 0x05, 0x00, 0x14, 0x7C, 0x00, 0x01]));
        assert!(ci.windows(4).any(|w| w == b"Duca"));
        // CS_CORE announces 1024x768 and TLS
        let p = ci.windows(4).position(|w| w == [0x01, 0xC0, 0xD8, 0x00]).unwrap(); // CS_CORE, 216 bytes
        assert_eq!(&ci[p + 4..p + 8], &[0x04, 0x00, 0x08, 0x00]); // version
        assert_eq!(&ci[p + 8..p + 12], &[0x00, 0x04, 0x00, 0x03]); // 1024 x 768
    }

    #[test]
    fn connect_response_blocks_parse() {
        let mut r = alloc::vec![0x7F, 0x66, 0x5A, 0x0A, 0x01, 0x00, 0x02, 0x01, 0x00, 0x30, 0x1A, 0x02, 0x01, 0x16];
        r.extend_from_slice(b"McDn");
        let mut blocks = Vec::new();
        blocks.extend_from_slice(&[0x01, 0x0C, 0x08, 0x00, 0x04, 0x00, 0x08, 0x00]); // SC_CORE v
        blocks.extend_from_slice(&[0x02, 0x0C, 0x0C, 0x00, 0, 0, 0, 0, 0, 0, 0, 0]); // SC_SECURITY none
        blocks.extend_from_slice(&[0x03, 0x0C, 0x0A, 0x00, 0xEB, 0x03, 0x01, 0x00, 0xEC, 0x03]); // SC_NET io=1003, 1 channel 1004
        r.push(blocks.len() as u8);
        r.extend_from_slice(&blocks);
        let sd = parse_connect_response(&r).unwrap();
        assert_eq!(sd.version, 0x0008_0004);
        assert_eq!(sd.encryption_level, 0);
        assert_eq!(sd.io_channel, 1003);
        assert_eq!(sd.channel_ids, alloc::vec![1004]);
    }

    #[test]
    fn domain_pdus() {
        assert_eq!(erect_domain_request(), [0x04, 1, 0, 1, 0]);
        assert_eq!(parse_attach_user_confirm(&[0x2E, 0x00, 0x00, 0x06]), Some(1007));
        assert_eq!(channel_join_request(1007, 1003), [0x38, 0, 6, 0x03, 0xEB]);
        assert_eq!(parse_channel_join_confirm(&[0x3E, 0, 0, 6, 0x03, 0xEB, 0x03, 0xEB]), Some(1003));
        let sdr = send_data_request(1007, 1003, &[1, 2, 3]);
        assert_eq!(sdr, [0x64, 0, 6, 0x03, 0xEB, 0x70, 3, 1, 2, 3]);
        let sdi = [0x68, 0, 6, 0x03, 0xEB, 0x70, 0x80, 0x03, 9, 8, 7];
        assert_eq!(parse_send_data_indication(&sdi), Some((1003, &[9u8, 8, 7][..])));
        assert!(is_disconnect(&[0x21, 0x80]));
    }
}
