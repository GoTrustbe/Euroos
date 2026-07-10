//! DHCPv6 (RFC 8415) — stateful IPv6 address configuration, the counterpart to
//! the IPv4 [`crate::dhcp`] and the stateless SLAAC path in [`crate::ipv6`].
//!
//! A client sends **Solicit** to the `ff02::1:2` relay/server multicast group
//! (from UDP 546 to 547); the server answers **Advertise** with an offered
//! address inside an IA_NA option; the client confirms with **Request** and the
//! server replies. This module builds/parses those messages. Pure `no_std`.

use alloc::vec::Vec;

// Message types.
pub const MSG_SOLICIT: u8 = 1;
pub const MSG_ADVERTISE: u8 = 2;
pub const MSG_REQUEST: u8 = 3;
pub const MSG_REPLY: u8 = 7;

// Option codes.
const OPT_CLIENTID: u16 = 1;
const OPT_SERVERID: u16 = 2;
const OPT_IA_NA: u16 = 3;
const OPT_IAADDR: u16 = 5;
const OPT_ELAPSED: u16 = 8;

/// Client & server ports; the all-DHCP-servers multicast group.
pub const CLIENT_PORT: u16 = 546;
pub const SERVER_PORT: u16 = 547;
pub const ALL_SERVERS: [u8; 16] = [0xff, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 2];

fn opt(out: &mut Vec<u8>, code: u16, data: &[u8]) {
    out.extend_from_slice(&code.to_be_bytes());
    out.extend_from_slice(&(data.len() as u16).to_be_bytes());
    out.extend_from_slice(data);
}

fn header(out: &mut Vec<u8>, msg_type: u8, txid: [u8; 3]) {
    out.push(msg_type);
    out.extend_from_slice(&txid);
}

/// An IA_NA option body wrapping (optionally) an offered/assigned IA Address.
fn ia_na(iaid: u32, addr: Option<([u8; 16], u32, u32)>) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&iaid.to_be_bytes());
    b.extend_from_slice(&0u32.to_be_bytes()); // T1
    b.extend_from_slice(&0u32.to_be_bytes()); // T2
    if let Some((ip, pref, valid)) = addr {
        let mut iaaddr = Vec::new();
        iaaddr.extend_from_slice(&ip);
        iaaddr.extend_from_slice(&pref.to_be_bytes());
        iaaddr.extend_from_slice(&valid.to_be_bytes());
        opt(&mut b, OPT_IAADDR, &iaaddr);
    }
    b
}

/// Build a **Solicit**: request an address (client DUID + empty IA_NA).
pub fn build_solicit(txid: [u8; 3], client_duid: &[u8], iaid: u32) -> Vec<u8> {
    let mut b = Vec::new();
    header(&mut b, MSG_SOLICIT, txid);
    opt(&mut b, OPT_CLIENTID, client_duid);
    opt(&mut b, OPT_ELAPSED, &[0, 0]);
    opt(&mut b, OPT_IA_NA, &ia_na(iaid, None));
    b
}

/// Build a **Request**: confirm the offered address (client + server DUID +
/// IA_NA carrying the address from the Advertise).
pub fn build_request(txid: [u8; 3], client_duid: &[u8], server_duid: &[u8], iaid: u32, addr: [u8; 16], valid: u32) -> Vec<u8> {
    let mut b = Vec::new();
    header(&mut b, MSG_REQUEST, txid);
    opt(&mut b, OPT_CLIENTID, client_duid);
    opt(&mut b, OPT_SERVERID, server_duid);
    opt(&mut b, OPT_IA_NA, &ia_na(iaid, Some((addr, valid / 2, valid))));
    b
}

/// A parsed DHCPv6 message (the fields a client cares about).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Msg {
    pub msg_type: u8,
    pub txid: [u8; 3],
    pub client_duid: Vec<u8>,
    pub server_duid: Vec<u8>,
    /// The offered/assigned (address, valid-lifetime), if present.
    pub ia_addr: Option<([u8; 16], u32)>,
}

fn parse_ia_na(data: &[u8]) -> Option<([u8; 16], u32)> {
    // IAID(4) T1(4) T2(4), then options.
    let mut p = 12;
    while p + 4 <= data.len() {
        let code = u16::from_be_bytes([data[p], data[p + 1]]);
        let len = u16::from_be_bytes([data[p + 2], data[p + 3]]) as usize;
        p += 4;
        let body = data.get(p..p + len)?;
        if code == OPT_IAADDR && body.len() >= 24 {
            let ip: [u8; 16] = body[..16].try_into().ok()?;
            let valid = u32::from_be_bytes([body[20], body[21], body[22], body[23]]);
            return Some((ip, valid));
        }
        p += len;
    }
    None
}

/// Parse a DHCPv6 message. `None` on malformed input.
pub fn parse(msg: &[u8]) -> Option<Msg> {
    if msg.len() < 4 {
        return None;
    }
    let msg_type = msg[0];
    let txid = [msg[1], msg[2], msg[3]];
    let mut out = Msg { msg_type, txid, client_duid: Vec::new(), server_duid: Vec::new(), ia_addr: None };
    let mut p = 4;
    while p + 4 <= msg.len() {
        let code = u16::from_be_bytes([msg[p], msg[p + 1]]);
        let len = u16::from_be_bytes([msg[p + 2], msg[p + 3]]) as usize;
        p += 4;
        let body = msg.get(p..p + len)?;
        match code {
            OPT_CLIENTID => out.client_duid = body.to_vec(),
            OPT_SERVERID => out.server_duid = body.to_vec(),
            OPT_IA_NA => out.ia_addr = parse_ia_na(body),
            _ => {}
        }
        p += len;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solicit_carries_client_and_ia() {
        let duid = [0xDE, 0xAD, 0xBE, 0xEF];
        let s = build_solicit([1, 2, 3], &duid, 0x1000);
        let m = parse(&s).unwrap();
        assert_eq!(m.msg_type, MSG_SOLICIT);
        assert_eq!(m.txid, [1, 2, 3]);
        assert_eq!(m.client_duid, duid);
    }

    #[test]
    fn advertise_then_request_roundtrip() {
        // A server "Advertise" offering 2001:db8::5.
        let addr = [0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 5];
        let client = [1u8, 2, 3, 4];
        let server = [9u8, 8, 7, 6];
        let mut adv = Vec::new();
        header(&mut adv, MSG_ADVERTISE, [1, 2, 3]);
        opt(&mut adv, OPT_CLIENTID, &client);
        opt(&mut adv, OPT_SERVERID, &server);
        opt(&mut adv, OPT_IA_NA, &ia_na(0x1000, Some((addr, 1800, 3600))));

        let m = parse(&adv).unwrap();
        assert_eq!(m.msg_type, MSG_ADVERTISE);
        assert_eq!(m.server_duid, server);
        assert_eq!(m.ia_addr, Some((addr, 3600)));

        // The client Requests the offered address.
        let req = build_request(m.txid, &client, &m.server_duid, 0x1000, addr, 3600);
        let rm = parse(&req).unwrap();
        assert_eq!(rm.msg_type, MSG_REQUEST);
        assert_eq!(rm.ia_addr, Some((addr, 3600)));
        assert_eq!(rm.server_duid, server);
    }

    #[test]
    fn malformed_is_none() {
        assert!(parse(&[1, 2]).is_none());
    }
}
