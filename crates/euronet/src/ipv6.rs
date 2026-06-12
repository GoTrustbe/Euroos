//! IPv6 (RFC 8200): 40-byte header + adres-helpers (SLAAC link-local via EUI-64,
//! solicited-node multicast, multicast→MAC mapping) en de pseudo-header-checksum
//! die ICMPv6/UDP nodig hebben.

use alloc::vec::Vec;

use crate::checksum::internet_checksum;
use crate::{NetError, NetResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ipv6Addr(pub [u8; 16]);

impl Ipv6Addr {
    pub const UNSPECIFIED: Ipv6Addr = Ipv6Addr([0; 16]);
    /// ff02::1 — alle nodes op de link.
    pub const ALL_NODES: Ipv6Addr =
        Ipv6Addr([0xff, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
    /// ff02::2 — alle routers op de link.
    pub const ALL_ROUTERS: Ipv6Addr =
        Ipv6Addr([0xff, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2]);

    pub fn is_multicast(&self) -> bool {
        self.0[0] == 0xff
    }

    /// SLAAC: link-local adres (fe80::/64) uit het MAC-adres via EUI-64.
    pub fn link_local_from_mac(mac: [u8; 6]) -> Ipv6Addr {
        let mut a = [0u8; 16];
        a[0] = 0xfe;
        a[1] = 0x80;
        // EUI-64: mac[0..3] ff fe mac[3..6], met de U/L-bit (bit 1) geflipt.
        a[8] = mac[0] ^ 0x02;
        a[9] = mac[1];
        a[10] = mac[2];
        a[11] = 0xff;
        a[12] = 0xfe;
        a[13] = mac[3];
        a[14] = mac[4];
        a[15] = mac[5];
        Ipv6Addr(a)
    }

    /// Vorm een adres uit een /64-prefix (eerste 8 bytes) + onze interface-id.
    pub fn from_prefix(prefix: [u8; 8], link_local: &Ipv6Addr) -> Ipv6Addr {
        let mut a = [0u8; 16];
        a[..8].copy_from_slice(&prefix);
        a[8..].copy_from_slice(&link_local.0[8..]);
        Ipv6Addr(a)
    }

    /// Solicited-node multicast: ff02::1:ffXX:XXXX (laatste 3 bytes van het adres).
    pub fn solicited_node(&self) -> Ipv6Addr {
        let mut a = [0u8; 16];
        a[0] = 0xff;
        a[1] = 0x02;
        a[11] = 0x01;
        a[12] = 0xff;
        a[13] = self.0[13];
        a[14] = self.0[14];
        a[15] = self.0[15];
        Ipv6Addr(a)
    }

    /// Ethernet-MAC voor een IPv6-multicast-adres: 33:33 + laatste 4 bytes.
    pub fn multicast_mac(&self) -> [u8; 6] {
        [0x33, 0x33, self.0[12], self.0[13], self.0[14], self.0[15]]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ipv6Header {
    pub next_header: u8,
    pub hop_limit: u8,
    pub src: Ipv6Addr,
    pub dst: Ipv6Addr,
    pub payload_len: u16,
}

impl Ipv6Header {
    pub const LEN: usize = 40;

    pub fn build(&self, payload: &[u8]) -> Vec<u8> {
        let mut h = Vec::with_capacity(Self::LEN + payload.len());
        h.extend_from_slice(&[0x60, 0, 0, 0]); // versie 6, traffic class/flow = 0
        h.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        h.push(self.next_header);
        h.push(self.hop_limit);
        h.extend_from_slice(&self.src.0);
        h.extend_from_slice(&self.dst.0);
        h.extend_from_slice(payload);
        h
    }

    pub fn parse(buf: &[u8]) -> NetResult<(Self, &[u8])> {
        if buf.len() < Self::LEN {
            return Err(NetError::TooShort);
        }
        if buf[0] >> 4 != 6 {
            return Err(NetError::Malformed);
        }
        let payload_len = u16::from_be_bytes([buf[4], buf[5]]);
        let mut src = [0u8; 16];
        let mut dst = [0u8; 16];
        src.copy_from_slice(&buf[8..24]);
        dst.copy_from_slice(&buf[24..40]);
        let end = (Self::LEN + payload_len as usize).min(buf.len());
        Ok((
            Self {
                next_header: buf[6],
                hop_limit: buf[7],
                src: Ipv6Addr(src),
                dst: Ipv6Addr(dst),
                payload_len,
            },
            &buf[Self::LEN..end],
        ))
    }
}

/// Checksum over de IPv6-pseudo-header + upper-layer-payload (voor ICMPv6/UDP).
pub fn pseudo_checksum(src: Ipv6Addr, dst: Ipv6Addr, next_header: u8, payload: &[u8]) -> u16 {
    let mut buf = Vec::with_capacity(40 + payload.len());
    buf.extend_from_slice(&src.0);
    buf.extend_from_slice(&dst.0);
    buf.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    buf.extend_from_slice(&[0, 0, 0, next_header]);
    buf.extend_from_slice(payload);
    internet_checksum(&buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eui64_link_local() {
        let ll = Ipv6Addr::link_local_from_mac([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
        // fe80::5054: ff:fe12:3456  (52 ^ 0x02 = 50)
        assert_eq!(ll.0[0], 0xfe);
        assert_eq!(ll.0[1], 0x80);
        assert_eq!(&ll.0[8..], &[0x50, 0x54, 0x00, 0xff, 0xfe, 0x12, 0x34, 0x56]);
    }

    #[test]
    fn solicited_and_mcast_mac() {
        let ll = Ipv6Addr::link_local_from_mac([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
        let sn = ll.solicited_node();
        assert_eq!(&sn.0[0..2], &[0xff, 0x02]);
        assert_eq!(&sn.0[11..], &[0x01, 0xff, 0x12, 0x34, 0x56]);
        assert_eq!(sn.multicast_mac(), [0x33, 0x33, 0xff, 0x12, 0x34, 0x56]);
        assert_eq!(Ipv6Addr::ALL_ROUTERS.multicast_mac(), [0x33, 0x33, 0, 0, 0, 2]);
    }

    #[test]
    fn header_roundtrip() {
        let h = Ipv6Header {
            next_header: 58,
            hop_limit: 255,
            src: Ipv6Addr::link_local_from_mac([2, 0, 0, 0, 0, 1]),
            dst: Ipv6Addr::ALL_ROUTERS,
            payload_len: 0,
        };
        let bytes = h.build(&[1, 2, 3, 4]);
        let (p, pl) = Ipv6Header::parse(&bytes).unwrap();
        assert_eq!(p.next_header, 58);
        assert_eq!(p.hop_limit, 255);
        assert_eq!(pl, &[1, 2, 3, 4]);
    }
}
