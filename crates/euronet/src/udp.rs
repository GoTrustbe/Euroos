//! UDP (RFC 768), including the IPv4 pseudo-header checksum.

use alloc::vec::Vec;

use crate::checksum::internet_checksum;
use crate::ipv4::Ipv4Addr;
use crate::{NetError, NetResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UdpDatagram {
    pub src_port: u16,
    pub dst_port: u16,
    pub payload: Vec<u8>,
}

impl UdpDatagram {
    pub const HEADER_LEN: usize = 8;

    /// Build the pseudo-header for the checksum computation.
    fn pseudo(src: Ipv4Addr, dst: Ipv4Addr, udp_len: u16) -> [u8; 12] {
        let mut p = [0u8; 12];
        p[0..4].copy_from_slice(&src.0);
        p[4..8].copy_from_slice(&dst.0);
        p[8] = 0;
        p[9] = 17; // protocol UDP
        p[10..12].copy_from_slice(&udp_len.to_be_bytes());
        p
    }

    pub fn build(&self, src: Ipv4Addr, dst: Ipv4Addr) -> Vec<u8> {
        let udp_len = (Self::HEADER_LEN + self.payload.len()) as u16;
        let mut seg = Vec::with_capacity(udp_len as usize);
        seg.extend_from_slice(&self.src_port.to_be_bytes());
        seg.extend_from_slice(&self.dst_port.to_be_bytes());
        seg.extend_from_slice(&udp_len.to_be_bytes());
        seg.extend_from_slice(&[0, 0]); // checksum placeholder

        seg.extend_from_slice(&self.payload);

        // Checksum over pseudo-header + segment.
        let mut buf = Vec::with_capacity(12 + seg.len());
        buf.extend_from_slice(&Self::pseudo(src, dst, udp_len));
        buf.extend_from_slice(&seg);
        let mut cs = internet_checksum(&buf);
        if cs == 0 {
            cs = 0xFFFF; // 0 means "no checksum"; use all-ones.
        }
        seg[6..8].copy_from_slice(&cs.to_be_bytes());
        seg
    }

    /// Parse + verify the checksum (requires src/dst for the pseudo-header).
    pub fn parse(buf: &[u8], src: Ipv4Addr, dst: Ipv4Addr) -> NetResult<Self> {
        if buf.len() < Self::HEADER_LEN {
            return Err(NetError::TooShort);
        }
        let length = u16::from_be_bytes([buf[4], buf[5]]) as usize;
        if length < Self::HEADER_LEN || length > buf.len() {
            return Err(NetError::Malformed);
        }
        let checksum = u16::from_be_bytes([buf[6], buf[7]]);
        if checksum != 0 {
            let mut v = Vec::with_capacity(12 + length);
            v.extend_from_slice(&Self::pseudo(src, dst, length as u16));
            v.extend_from_slice(&buf[..length]);
            if internet_checksum(&v) != 0 {
                return Err(NetError::BadChecksum);
            }
        }
        Ok(Self {
            src_port: u16::from_be_bytes([buf[0], buf[1]]),
            dst_port: u16::from_be_bytes([buf[2], buf[3]]),
            payload: buf[Self::HEADER_LEN..length].to_vec(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_parse_roundtrip_met_checksum() {
        let src = Ipv4Addr::new(192, 168, 1, 10);
        let dst = Ipv4Addr::new(192, 168, 1, 1);
        let dg = UdpDatagram {
            src_port: 5353,
            dst_port: 53,
            payload: b"DNS-query".to_vec(),
        };
        let seg = dg.build(src, dst);
        let parsed = UdpDatagram::parse(&seg, src, dst).unwrap();
        assert_eq!(parsed, dg);
    }

    #[test]
    fn verkeerde_pseudo_header_faalt_checksum() {
        let src = Ipv4Addr::new(10, 0, 0, 1);
        let dst = Ipv4Addr::new(10, 0, 0, 2);
        let dg = UdpDatagram {
            src_port: 1000,
            dst_port: 2000,
            payload: b"x".to_vec(),
        };
        let seg = dg.build(src, dst);
        // Parse with WRONG source address -> different pseudo-header -> checksum error.
        let wrong = Ipv4Addr::new(10, 0, 0, 9);
        assert_eq!(
            UdpDatagram::parse(&seg, wrong, dst).unwrap_err(),
            NetError::BadChecksum
        );
    }
}
