//! ICMPv6 (RFC 4443) + Neighbor Discovery (RFC 4861): echo, Router Solicitation,
//! Neighbor Solicitation, en het parsen van Router/Neighbor Advertisements. De
//! checksum gebruikt de IPv6-pseudo-header.

use alloc::vec::Vec;

use crate::ipv6::{pseudo_checksum, Ipv6Addr};

pub const ECHO_REQUEST: u8 = 128;
pub const ECHO_REPLY: u8 = 129;
pub const ROUTER_SOLICIT: u8 = 133;
pub const ROUTER_ADVERT: u8 = 134;
pub const NEIGHBOR_SOLICIT: u8 = 135;
pub const NEIGHBOR_ADVERT: u8 = 136;

const NEXT_ICMPV6: u8 = 58;

/// Zet de checksum (over de pseudo-header) in een opgebouwd ICMPv6-bericht.
fn finalize(mut msg: Vec<u8>, src: Ipv6Addr, dst: Ipv6Addr) -> Vec<u8> {
    msg[2] = 0;
    msg[3] = 0;
    let cs = pseudo_checksum(src, dst, NEXT_ICMPV6, &msg);
    msg[2..4].copy_from_slice(&cs.to_be_bytes());
    msg
}

/// ICMPv6 Echo Request (ping6).
pub fn echo_request(id: u16, seq: u16, data: &[u8], src: Ipv6Addr, dst: Ipv6Addr) -> Vec<u8> {
    let mut m = alloc::vec![ECHO_REQUEST, 0, 0, 0];
    m.extend_from_slice(&id.to_be_bytes());
    m.extend_from_slice(&seq.to_be_bytes());
    m.extend_from_slice(data);
    finalize(m, src, dst)
}

/// Router Solicitation (met source link-layer-optie) — vraag om een RA.
pub fn router_solicit(src_mac: [u8; 6], src: Ipv6Addr, dst: Ipv6Addr) -> Vec<u8> {
    let mut m = alloc::vec![ROUTER_SOLICIT, 0, 0, 0, 0, 0, 0, 0];
    m.push(1); // optie: source link-layer address
    m.push(1); // lengte in eenheden van 8 bytes
    m.extend_from_slice(&src_mac);
    finalize(m, src, dst)
}

/// Neighbor Solicitation — "wie heeft `target`?" (de IPv6-tegenhanger van ARP).
pub fn neighbor_solicit(target: Ipv6Addr, src_mac: [u8; 6], src: Ipv6Addr, dst: Ipv6Addr) -> Vec<u8> {
    let mut m = alloc::vec![NEIGHBOR_SOLICIT, 0, 0, 0, 0, 0, 0, 0];
    m.extend_from_slice(&target.0);
    m.push(1);
    m.push(1);
    m.extend_from_slice(&src_mac);
    finalize(m, src, dst)
}

/// Het ICMPv6-type van een bericht.
pub fn msg_type(buf: &[u8]) -> Option<u8> {
    buf.first().copied()
}

/// Parse een Router Advertisement: (prefix /64, router-MAC) uit de opties.
pub fn ra_info(buf: &[u8]) -> (Option<[u8; 8]>, Option<[u8; 6]>) {
    let mut prefix = None;
    let mut mac = None;
    let mut i = 16; // RA-header is 16 bytes
    while i + 2 <= buf.len() {
        let t = buf[i];
        let l = buf[i + 1] as usize * 8;
        if l == 0 || i + l > buf.len() {
            break;
        }
        if t == 3 && l >= 32 {
            // Prefix Information: prefix begint op offset +16 binnen de optie.
            let mut p = [0u8; 8];
            p.copy_from_slice(&buf[i + 16..i + 24]);
            prefix = Some(p);
        } else if t == 1 && l >= 8 {
            let mut m = [0u8; 6];
            m.copy_from_slice(&buf[i + 2..i + 8]);
            mac = Some(m);
        }
        i += l;
    }
    (prefix, mac)
}

/// Parse een Neighbor Advertisement: target link-layer-adres (MAC).
pub fn na_mac(buf: &[u8]) -> Option<[u8; 6]> {
    let mut i = 24; // NA-header is 24 bytes (type,code,cksum,flags(4),target(16))
    while i + 2 <= buf.len() {
        let t = buf[i];
        let l = buf[i + 1] as usize * 8;
        if l == 0 || i + l > buf.len() {
            break;
        }
        if t == 2 && l >= 8 {
            let mut m = [0u8; 6];
            m.copy_from_slice(&buf[i + 2..i + 8]);
            return Some(m);
        }
        i += l;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echo_has_valid_checksum() {
        let src = Ipv6Addr::link_local_from_mac([2, 0, 0, 0, 0, 1]);
        let dst = Ipv6Addr::ALL_NODES;
        let m = echo_request(0x1234, 1, b"hoi", src, dst);
        assert_eq!(m[0], ECHO_REQUEST);
        // checksum over pseudo+msg moet 0 verifiëren
        assert_eq!(pseudo_checksum(src, dst, 58, &m), 0);
    }

    #[test]
    fn parse_ra() {
        // RA-header (16) + prefix-optie (type 3, len 4) + SLLA (type 1, len 1).
        let mut b = alloc::vec![134u8, 0, 0, 0, 64, 0, 0x07, 0x08, 0, 0, 0, 0, 0, 0, 0, 0];
        // prefix info: type3 len4 prefixlen64 flags valid(4) pref(4) res(4) prefix(16)
        b.extend_from_slice(&[3, 4, 64, 0xc0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        b.extend_from_slice(&[0xfe, 0xc0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        // source link-layer
        b.extend_from_slice(&[1, 1, 0x52, 0x55, 0x0a, 0, 0x02, 0x02]);
        let (prefix, mac) = ra_info(&b);
        assert_eq!(prefix, Some([0xfe, 0xc0, 0, 0, 0, 0, 0, 0]));
        assert_eq!(mac, Some([0x52, 0x55, 0x0a, 0, 0x02, 0x02]));
    }
}
