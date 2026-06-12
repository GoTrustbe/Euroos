//! Minimale TCP (RFC 793): segment-header bouwen/parsen met de IPv4-pseudo-header-
//! checksum. Genoeg voor een eenvoudige client: handshake → data → teardown.

use alloc::vec::Vec;

use crate::checksum::internet_checksum;
use crate::ipv4::Ipv4Addr;
use crate::{NetError, NetResult};

pub const FIN: u8 = 0x01;
pub const SYN: u8 = 0x02;
pub const RST: u8 = 0x04;
pub const PSH: u8 = 0x08;
pub const ACK: u8 = 0x10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TcpSegment {
    pub src_port: u16,
    pub dst_port: u16,
    pub seq: u32,
    pub ack: u32,
    pub flags: u8,
    pub window: u16,
    pub payload: Vec<u8>,
}

impl TcpSegment {
    pub const HEADER_LEN: usize = 20;

    fn pseudo(src: Ipv4Addr, dst: Ipv4Addr, tcp_len: u16) -> [u8; 12] {
        let mut p = [0u8; 12];
        p[0..4].copy_from_slice(&src.0);
        p[4..8].copy_from_slice(&dst.0);
        p[9] = 6; // protocol TCP
        p[10..12].copy_from_slice(&tcp_len.to_be_bytes());
        p
    }

    pub fn build(&self, src: Ipv4Addr, dst: Ipv4Addr) -> Vec<u8> {
        let tcp_len = (Self::HEADER_LEN + self.payload.len()) as u16;
        let mut seg = Vec::with_capacity(tcp_len as usize);
        seg.extend_from_slice(&self.src_port.to_be_bytes());
        seg.extend_from_slice(&self.dst_port.to_be_bytes());
        seg.extend_from_slice(&self.seq.to_be_bytes());
        seg.extend_from_slice(&self.ack.to_be_bytes());
        // data offset (5 woorden = 20 bytes) << 12 | flags
        let off_flags: u16 = (5u16 << 12) | (self.flags as u16);
        seg.extend_from_slice(&off_flags.to_be_bytes());
        seg.extend_from_slice(&self.window.to_be_bytes());
        seg.extend_from_slice(&[0, 0]); // checksum-placeholder
        seg.extend_from_slice(&[0, 0]); // urgent pointer
        seg.extend_from_slice(&self.payload);

        let mut buf = Vec::with_capacity(12 + seg.len());
        buf.extend_from_slice(&Self::pseudo(src, dst, tcp_len));
        buf.extend_from_slice(&seg);
        let cs = internet_checksum(&buf);
        seg[16..18].copy_from_slice(&cs.to_be_bytes());
        seg
    }

    pub fn parse(buf: &[u8]) -> NetResult<Self> {
        if buf.len() < Self::HEADER_LEN {
            return Err(NetError::TooShort);
        }
        let off = ((buf[12] >> 4) as usize) * 4;
        if off < Self::HEADER_LEN || off > buf.len() {
            return Err(NetError::Malformed);
        }
        Ok(Self {
            src_port: u16::from_be_bytes([buf[0], buf[1]]),
            dst_port: u16::from_be_bytes([buf[2], buf[3]]),
            seq: u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]),
            ack: u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]),
            flags: buf[13] & 0x3f,
            window: u16::from_be_bytes([buf[14], buf[15]]),
            payload: buf[off..].to_vec(),
        })
    }

    pub fn has(&self, flag: u8) -> bool {
        self.flags & flag != 0
    }

    /// Bouw het RST-segment dat een gesloten poort (geen luisteraar) hoort terug te
    /// sturen voor een binnenkomend `incoming`-segment, volgens RFC 793 §3.4. De
    /// poorten worden gespiegeld. Geeft `None` als `incoming` zélf al een RST is —
    /// op een reset hoort men nooit te antwoorden (anders een eindeloze RST-storm).
    ///
    /// - Heeft `incoming` ACK gezet, dan: `seq = incoming.ack`, vlaggen = `RST` (geen ACK).
    /// - Anders: `seq = 0`, `ack = incoming.seq + segmentlengte` (SYN en FIN tellen elk
    ///   als 1 sequentienummer), vlaggen = `RST | ACK`.
    pub fn reset_to(incoming: &TcpSegment) -> Option<TcpSegment> {
        if incoming.has(RST) {
            return None;
        }
        let (seq, ack, flags) = if incoming.has(ACK) {
            (incoming.ack, 0, RST)
        } else {
            let seg_len = incoming.payload.len() as u32
                + incoming.has(SYN) as u32
                + incoming.has(FIN) as u32;
            (0, incoming.seq.wrapping_add(seg_len), RST | ACK)
        };
        Some(TcpSegment {
            src_port: incoming.dst_port,
            dst_port: incoming.src_port,
            seq,
            ack,
            flags,
            window: 0,
            payload: Vec::new(),
        })
    }

    /// Verifieer de TCP-checksum van een rauw segment, inclusief de IPv4-pseudo-
    /// header (vereist de bron/doel-IP's). True = geldig. Een segment met een foute
    /// checksum is corrupt onderweg en hoort verworpen te worden.
    pub fn verify_checksum(seg: &[u8], src: Ipv4Addr, dst: Ipv4Addr) -> bool {
        if seg.len() < Self::HEADER_LEN {
            return false;
        }
        let mut buf = Vec::with_capacity(12 + seg.len());
        buf.extend_from_slice(&Self::pseudo(src, dst, seg.len() as u16));
        buf.extend_from_slice(seg);
        internet_checksum(&buf) == 0
    }

    /// Zoals [`parse`], maar verifieert eerst de checksum (pseudo-header + segment)
    /// en weigert een corrupt segment met `BadChecksum`.
    pub fn parse_checked(seg: &[u8], src: Ipv4Addr, dst: Ipv4Addr) -> NetResult<Self> {
        if !Self::verify_checksum(seg, src, dst) {
            return Err(NetError::BadChecksum);
        }
        Self::parse(seg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syn_roundtrip() {
        let s = TcpSegment {
            src_port: 50000,
            dst_port: 80,
            seq: 1000,
            ack: 0,
            flags: SYN,
            window: 64240,
            payload: Vec::new(),
        };
        let a = Ipv4Addr([10, 0, 2, 15]);
        let b = Ipv4Addr([93, 184, 216, 34]);
        let bytes = s.build(a, b);
        // checksum over pseudo + segment moet 0 verifiëren
        let mut v = Vec::new();
        v.extend_from_slice(&TcpSegment::pseudo(a, b, bytes.len() as u16));
        v.extend_from_slice(&bytes);
        assert_eq!(internet_checksum(&v), 0);
        let p = TcpSegment::parse(&bytes).unwrap();
        assert_eq!(p.dst_port, 80);
        assert!(p.has(SYN));
        assert_eq!(p.seq, 1000);
    }

    #[test]
    fn parse_checked_weigert_corrupt_segment() {
        let a = Ipv4Addr([10, 0, 0, 1]);
        let b = Ipv4Addr([10, 0, 0, 2]);
        let s = TcpSegment {
            src_port: 1234,
            dst_port: 80,
            seq: 7,
            ack: 0,
            flags: SYN | ACK,
            window: 64240,
            payload: b"euroos".to_vec(),
        };
        let bytes = s.build(a, b);
        // Geldig segment: checksum klopt en parse_checked slaagt.
        assert!(TcpSegment::verify_checksum(&bytes, a, b));
        assert!(TcpSegment::parse_checked(&bytes, a, b).is_ok());
        // Eén header-byte flippen → checksum faalt → BadChecksum.
        let mut corrupt = bytes.clone();
        corrupt[4] ^= 0xFF;
        assert!(!TcpSegment::verify_checksum(&corrupt, a, b));
        assert_eq!(TcpSegment::parse_checked(&corrupt, a, b), Err(NetError::BadChecksum));
        // Verkeerd bron/doel-IP → andere pseudo-header → checksum faalt.
        assert!(!TcpSegment::verify_checksum(&bytes, a, Ipv4Addr([10, 0, 0, 9])));
    }

    #[test]
    fn parse_with_payload_and_offset() {
        let s = TcpSegment { src_port: 1, dst_port: 2, seq: 5, ack: 9, flags: PSH | ACK, window: 100, payload: b"hi".to_vec() };
        let bytes = s.build(Ipv4Addr([1, 1, 1, 1]), Ipv4Addr([2, 2, 2, 2]));
        let p = TcpSegment::parse(&bytes).unwrap();
        assert!(p.has(PSH) && p.has(ACK));
        assert_eq!(p.payload, b"hi");
        assert_eq!(p.ack, 9);
    }

    #[test]
    fn reset_voor_syn_op_gesloten_poort() {
        // Een SYN (geen ACK) naar een dichte poort → RST|ACK, seq=0, ack=seq+1.
        let syn = TcpSegment {
            src_port: 51000, dst_port: 9999, seq: 4242, ack: 0,
            flags: SYN, window: 64240, payload: Vec::new(),
        };
        let rst = TcpSegment::reset_to(&syn).unwrap();
        assert!(rst.has(RST) && rst.has(ACK));
        assert_eq!(rst.src_port, 9999); // poorten gespiegeld
        assert_eq!(rst.dst_port, 51000);
        assert_eq!(rst.seq, 0);
        assert_eq!(rst.ack, 4243); // SYN telt als 1
    }

    #[test]
    fn reset_voor_ack_segment_spiegelt_seq() {
        // Een binnenkomend segment mét ACK → RST (geen ACK), seq = incoming.ack.
        let seg = TcpSegment {
            src_port: 40000, dst_port: 9999, seq: 1, ack: 7777,
            flags: ACK, window: 100, payload: Vec::new(),
        };
        let rst = TcpSegment::reset_to(&seg).unwrap();
        assert_eq!(rst.flags, RST);
        assert!(!rst.has(ACK));
        assert_eq!(rst.seq, 7777);
    }

    #[test]
    fn nooit_resetten_op_een_reset() {
        // Op een RST hoort men nooit te antwoorden — anders een RST-storm.
        let rst_in = TcpSegment {
            src_port: 1, dst_port: 2, seq: 0, ack: 0,
            flags: RST, window: 0, payload: Vec::new(),
        };
        assert!(TcpSegment::reset_to(&rst_in).is_none());
    }
}
