//! Ethernet II frame-header (14 bytes).

use alloc::vec::Vec;

use crate::{NetError, NetResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MacAddr(pub [u8; 6]);

impl MacAddr {
    pub const BROADCAST: MacAddr = MacAddr([0xFF; 6]);
    pub const ZERO: MacAddr = MacAddr([0x00; 6]);

    pub fn is_broadcast(self) -> bool {
        self == Self::BROADCAST
    }
    pub fn is_multicast(self) -> bool {
        self.0[0] & 0x01 != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EtherType {
    Ipv4,
    Ipv6,
    Arp,
    Other(u16),
}

impl EtherType {
    pub fn from_u16(v: u16) -> Self {
        match v {
            0x0800 => Self::Ipv4,
            0x86DD => Self::Ipv6,
            0x0806 => Self::Arp,
            o => Self::Other(o),
        }
    }
    pub fn as_u16(self) -> u16 {
        match self {
            Self::Ipv4 => 0x0800,
            Self::Ipv6 => 0x86DD,
            Self::Arp => 0x0806,
            Self::Other(o) => o,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EthernetHeader {
    pub dst: MacAddr,
    pub src: MacAddr,
    pub ethertype: EtherType,
}

impl EthernetHeader {
    pub const LEN: usize = 14;

    /// Parse the header; returns (header, payload).
    pub fn parse(buf: &[u8]) -> NetResult<(Self, &[u8])> {
        if buf.len() < Self::LEN {
            return Err(NetError::TooShort);
        }
        let mut dst = [0u8; 6];
        let mut src = [0u8; 6];
        dst.copy_from_slice(&buf[0..6]);
        src.copy_from_slice(&buf[6..12]);
        let et = u16::from_be_bytes([buf[12], buf[13]]);
        Ok((
            Self {
                dst: MacAddr(dst),
                src: MacAddr(src),
                ethertype: EtherType::from_u16(et),
            },
            &buf[Self::LEN..],
        ))
    }

    pub fn build(&self, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(Self::LEN + payload.len());
        out.extend_from_slice(&self.dst.0);
        out.extend_from_slice(&self.src.0);
        out.extend_from_slice(&self.ethertype.as_u16().to_be_bytes());
        out.extend_from_slice(payload);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_en_roundtrip() {
        let h = EthernetHeader {
            dst: MacAddr::BROADCAST,
            src: MacAddr([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]),
            ethertype: EtherType::Arp,
        };
        let frame = h.build(&[0xAA, 0xBB]);
        let (parsed, payload) = EthernetHeader::parse(&frame).unwrap();
        assert_eq!(parsed, h);
        assert_eq!(payload, &[0xAA, 0xBB]);
        assert!(parsed.dst.is_broadcast());
    }

    #[test]
    fn te_kort() {
        assert_eq!(EthernetHeader::parse(&[0; 10]).unwrap_err(), NetError::TooShort);
    }
}
