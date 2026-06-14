//! ARP (RFC 826) for IPv4-over-Ethernet.

use alloc::vec::Vec;

use crate::ethernet::MacAddr;
use crate::ipv4::Ipv4Addr;
use crate::{NetError, NetResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArpOp {
    Request,
    Reply,
    Other(u16),
}

impl ArpOp {
    fn from_u16(v: u16) -> Self {
        match v {
            1 => Self::Request,
            2 => Self::Reply,
            o => Self::Other(o),
        }
    }
    fn as_u16(self) -> u16 {
        match self {
            Self::Request => 1,
            Self::Reply => 2,
            Self::Other(o) => o,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArpPacket {
    pub op: ArpOp,
    pub sender_mac: MacAddr,
    pub sender_ip: Ipv4Addr,
    pub target_mac: MacAddr,
    pub target_ip: Ipv4Addr,
}

impl ArpPacket {
    pub const LEN: usize = 28;

    pub fn parse(buf: &[u8]) -> NetResult<Self> {
        if buf.len() < Self::LEN {
            return Err(NetError::TooShort);
        }
        let htype = u16::from_be_bytes([buf[0], buf[1]]);
        let ptype = u16::from_be_bytes([buf[2], buf[3]]);
        // Ethernet (1) + IPv4 (0x0800) only, hlen 6, plen 4.
        if htype != 1 || ptype != 0x0800 || buf[4] != 6 || buf[5] != 4 {
            return Err(NetError::Unsupported);
        }
        let mut smac = [0u8; 6];
        let mut tmac = [0u8; 6];
        smac.copy_from_slice(&buf[8..14]);
        tmac.copy_from_slice(&buf[18..24]);
        Ok(Self {
            op: ArpOp::from_u16(u16::from_be_bytes([buf[6], buf[7]])),
            sender_mac: MacAddr(smac),
            sender_ip: Ipv4Addr([buf[14], buf[15], buf[16], buf[17]]),
            target_mac: MacAddr(tmac),
            target_ip: Ipv4Addr([buf[24], buf[25], buf[26], buf[27]]),
        })
    }

    pub fn build(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(Self::LEN);
        b.extend_from_slice(&1u16.to_be_bytes()); // Ethernet
        b.extend_from_slice(&0x0800u16.to_be_bytes()); // IPv4
        b.push(6);
        b.push(4);
        b.extend_from_slice(&self.op.as_u16().to_be_bytes());
        b.extend_from_slice(&self.sender_mac.0);
        b.extend_from_slice(&self.sender_ip.0);
        b.extend_from_slice(&self.target_mac.0);
        b.extend_from_slice(&self.target_ip.0);
        b
    }

    /// Build an ARP reply to an incoming request: "who has `my_ip`?".
    pub fn reply_to(request: &ArpPacket, my_mac: MacAddr) -> ArpPacket {
        ArpPacket {
            op: ArpOp::Reply,
            sender_mac: my_mac,
            sender_ip: request.target_ip,
            target_mac: request.sender_mac,
            target_ip: request.sender_ip,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrip() {
        let req = ArpPacket {
            op: ArpOp::Request,
            sender_mac: MacAddr([0x52, 0x54, 0, 1, 2, 3]),
            sender_ip: Ipv4Addr::new(192, 168, 1, 10),
            target_mac: MacAddr::ZERO,
            target_ip: Ipv4Addr::new(192, 168, 1, 1),
        };
        let parsed = ArpPacket::parse(&req.build()).unwrap();
        assert_eq!(parsed, req);
    }

    #[test]
    fn reply_wisselt_velden() {
        let req = ArpPacket {
            op: ArpOp::Request,
            sender_mac: MacAddr([1, 1, 1, 1, 1, 1]),
            sender_ip: Ipv4Addr::new(10, 0, 0, 5),
            target_mac: MacAddr::ZERO,
            target_ip: Ipv4Addr::new(10, 0, 0, 1),
        };
        let me = MacAddr([0xAA; 6]);
        let rep = ArpPacket::reply_to(&req, me);
        assert_eq!(rep.op, ArpOp::Reply);
        assert_eq!(rep.sender_mac, me);
        assert_eq!(rep.sender_ip, Ipv4Addr::new(10, 0, 0, 1));
        assert_eq!(rep.target_mac, MacAddr([1, 1, 1, 1, 1, 1]));
        assert_eq!(rep.target_ip, Ipv4Addr::new(10, 0, 0, 5));
    }
}
