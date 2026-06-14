//! IPv4 header (20 bytes without options) + header checksum.

use alloc::vec::Vec;

use crate::checksum::internet_checksum;
use crate::{NetError, NetResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Ipv4Addr(pub [u8; 4]);

impl Ipv4Addr {
    pub const BROADCAST: Self = Self([255, 255, 255, 255]);
    pub const LOOPBACK: Self = Self([127, 0, 0, 1]);
    pub const ZERO: Self = Self([0, 0, 0, 0]);

    pub fn new(a: u8, b: u8, c: u8, d: u8) -> Self {
        Self([a, b, c, d])
    }
    pub fn is_private(self) -> bool {
        matches!(self.0,
            [10, ..]
            | [172, 16..=31, ..]
            | [192, 168, ..])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Icmp,
    Tcp,
    Udp,
    Other(u8),
}

impl Protocol {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Icmp,
            6 => Self::Tcp,
            17 => Self::Udp,
            o => Self::Other(o),
        }
    }
    pub fn as_u8(self) -> u8 {
        match self {
            Self::Icmp => 1,
            Self::Tcp => 6,
            Self::Udp => 17,
            Self::Other(o) => o,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ipv4Header {
    pub protocol: Protocol,
    pub ttl: u8,
    pub src: Ipv4Addr,
    pub dst: Ipv4Addr,
    /// Total length (header + payload).
    pub total_length: u16,
    pub identification: u16,
}

impl Ipv4Header {
    pub const LEN: usize = 20;

    pub fn parse(buf: &[u8]) -> NetResult<(Self, &[u8])> {
        if buf.len() < Self::LEN {
            return Err(NetError::TooShort);
        }
        let version = buf[0] >> 4;
        let ihl = (buf[0] & 0x0F) as usize * 4;
        if version != 4 || ihl < Self::LEN || buf.len() < ihl {
            return Err(NetError::Malformed);
        }
        // Verify the header checksum.
        if internet_checksum(&buf[..ihl]) != 0 {
            return Err(NetError::BadChecksum);
        }
        // Fragmentation: this stack does not reassemble. A fragment (More-Fragments
        // set, or a non-zero fragment offset) carries only PART of the payload;
        // passing it as a complete packet to TCP/UDP would be wrong. Reject it.
        let flags_frag = u16::from_be_bytes([buf[6], buf[7]]);
        let more_fragments = flags_frag & 0x2000 != 0; // MF bit (13)
        let frag_offset = flags_frag & 0x1FFF; // lowest 13 bits
        if more_fragments || frag_offset != 0 {
            return Err(NetError::Malformed);
        }
        let total_length = u16::from_be_bytes([buf[2], buf[3]]);
        let hdr = Self {
            protocol: Protocol::from_u8(buf[9]),
            ttl: buf[8],
            src: Ipv4Addr([buf[12], buf[13], buf[14], buf[15]]),
            dst: Ipv4Addr([buf[16], buf[17], buf[18], buf[19]]),
            total_length,
            identification: u16::from_be_bytes([buf[4], buf[5]]),
        };
        let end = (total_length as usize).min(buf.len()).max(ihl);
        Ok((hdr, &buf[ihl..end]))
    }

    /// Build header + payload with the correct length and checksum.
    pub fn build(&self, payload: &[u8]) -> Vec<u8> {
        let total = (Self::LEN + payload.len()) as u16;
        let mut h = [0u8; Self::LEN];
        h[0] = (4 << 4) | 5; // version 4, IHL 5
        h[1] = 0; // DSCP/ECN
        h[2..4].copy_from_slice(&total.to_be_bytes());
        h[4..6].copy_from_slice(&self.identification.to_be_bytes());
        h[6..8].copy_from_slice(&0u16.to_be_bytes()); // flags/fragment
        h[8] = self.ttl;
        h[9] = self.protocol.as_u8();
        // h[10..12] checksum = 0 during computation
        h[12..16].copy_from_slice(&self.src.0);
        h[16..20].copy_from_slice(&self.dst.0);
        let cs = internet_checksum(&h);
        h[10..12].copy_from_slice(&cs.to_be_bytes());

        let mut out = Vec::with_capacity(Self::LEN + payload.len());
        out.extend_from_slice(&h);
        out.extend_from_slice(payload);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_parse_roundtrip_en_checksum() {
        let h = Ipv4Header {
            protocol: Protocol::Udp,
            ttl: 64,
            src: Ipv4Addr::new(192, 168, 1, 10),
            dst: Ipv4Addr::new(192, 168, 1, 1),
            total_length: 0, // set by build
            identification: 0xABCD,
        };
        let pkt = h.build(b"hallo");
        let (parsed, payload) = Ipv4Header::parse(&pkt).unwrap();
        assert_eq!(parsed.protocol, Protocol::Udp);
        assert_eq!(parsed.ttl, 64);
        assert_eq!(parsed.src, Ipv4Addr::new(192, 168, 1, 10));
        assert_eq!(parsed.total_length, 25);
        assert_eq!(payload, b"hallo");
    }

    #[test]
    fn corrupte_checksum_geweigerd() {
        let h = Ipv4Header {
            protocol: Protocol::Icmp,
            ttl: 64,
            src: Ipv4Addr::LOOPBACK,
            dst: Ipv4Addr::LOOPBACK,
            total_length: 0,
            identification: 1,
        };
        let mut pkt = h.build(&[]);
        pkt[12] ^= 0xFF; // change src → checksum no longer matches
        assert_eq!(Ipv4Header::parse(&pkt).unwrap_err(), NetError::BadChecksum);
    }

    #[test]
    fn fragment_wordt_geweigerd() {
        let h = Ipv4Header {
            protocol: Protocol::Udp,
            ttl: 64,
            src: Ipv4Addr::new(10, 0, 0, 1),
            dst: Ipv4Addr::new(10, 0, 0, 2),
            total_length: 0,
            identification: 7,
        };
        let pkt = h.build(b"payload");
        assert!(Ipv4Header::parse(&pkt).is_ok()); // non-fragmented → ok
        let ihl = (pkt[0] & 0x0F) as usize * 4;
        // (a) More-Fragments bit set (checksum recomputed) → rejected.
        let mut mf = pkt.clone();
        mf[6] = 0x20;
        mf[10] = 0;
        mf[11] = 0;
        let cs = internet_checksum(&mf[..ihl]);
        mf[10..12].copy_from_slice(&cs.to_be_bytes());
        assert_eq!(Ipv4Header::parse(&mf).unwrap_err(), NetError::Malformed);
        // (b) Non-zero fragment offset → rejected.
        let mut off = pkt.clone();
        off[7] = 1;
        off[10] = 0;
        off[11] = 0;
        let cs2 = internet_checksum(&off[..ihl]);
        off[10..12].copy_from_slice(&cs2.to_be_bytes());
        assert_eq!(Ipv4Header::parse(&off).unwrap_err(), NetError::Malformed);
    }

    #[test]
    fn prive_adressen() {
        assert!(Ipv4Addr::new(10, 0, 0, 1).is_private());
        assert!(Ipv4Addr::new(172, 16, 5, 4).is_private());
        assert!(Ipv4Addr::new(192, 168, 0, 1).is_private());
        assert!(!Ipv4Addr::new(8, 8, 8, 8).is_private());
    }
}
