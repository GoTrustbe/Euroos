//! ICMP echo (ping), RFC 792.

use alloc::vec::Vec;

use crate::checksum::internet_checksum;
use crate::{NetError, NetResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IcmpType {
    EchoReply,
    EchoRequest,
    Other(u8),
}

impl IcmpType {
    fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::EchoReply,
            8 => Self::EchoRequest,
            o => Self::Other(o),
        }
    }
    fn as_u8(self) -> u8 {
        match self {
            Self::EchoReply => 0,
            Self::EchoRequest => 8,
            Self::Other(o) => o,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IcmpEcho {
    pub kind: IcmpType,
    pub identifier: u16,
    pub sequence: u16,
    pub payload: Vec<u8>,
}

impl IcmpEcho {
    pub fn parse(buf: &[u8]) -> NetResult<Self> {
        if buf.len() < 8 {
            return Err(NetError::TooShort);
        }
        if internet_checksum(buf) != 0 {
            return Err(NetError::BadChecksum);
        }
        Ok(Self {
            kind: IcmpType::from_u8(buf[0]),
            identifier: u16::from_be_bytes([buf[4], buf[5]]),
            sequence: u16::from_be_bytes([buf[6], buf[7]]),
            payload: buf[8..].to_vec(),
        })
    }

    pub fn build(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(8 + self.payload.len());
        b.push(self.kind.as_u8());
        b.push(0); // code
        b.extend_from_slice(&[0, 0]); // checksum-placeholder
        b.extend_from_slice(&self.identifier.to_be_bytes());
        b.extend_from_slice(&self.sequence.to_be_bytes());
        b.extend_from_slice(&self.payload);
        let cs = internet_checksum(&b);
        b[2..4].copy_from_slice(&cs.to_be_bytes());
        b
    }

    /// Bouw de echo-reply die hoort bij een ontvangen echo-request.
    pub fn reply_to(req: &IcmpEcho) -> IcmpEcho {
        IcmpEcho {
            kind: IcmpType::EchoReply,
            identifier: req.identifier,
            sequence: req.sequence,
            payload: req.payload.clone(),
        }
    }
}

/// ICMP-foutmeldingen (RFC 792): de soort fout die we terugsturen wanneer een
/// pakket niet afgeleverd kon worden. Het ICMP-bericht draagt de eerste bytes
/// van het oorspronkelijke datagram terug, zodat de afzender het kan koppelen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IcmpError {
    /// Type 3 — bestemming onbereikbaar.
    DestUnreachable(UnreachCode),
    /// Type 11, code 0 — TTL verlopen onderweg.
    TimeExceeded,
}

/// Codes onder "Destination Unreachable" (type 3) die wij genereren.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnreachCode {
    /// Code 1 — geen route naar de host.
    Host,
    /// Code 3 — de poort is gesloten (geen luisteraar).
    Port,
}

impl IcmpError {
    fn type_code(self) -> (u8, u8) {
        match self {
            Self::DestUnreachable(UnreachCode::Host) => (3, 1),
            Self::DestUnreachable(UnreachCode::Port) => (3, 3),
            Self::TimeExceeded => (11, 0),
        }
    }

    /// Bouw het ICMP-fout-payload (zónder IP-header) voor het datagram dat de
    /// fout veroorzaakte. RFC 792 schrijft voor: de IP-header + de eerste 8 bytes
    /// van de oorspronkelijke data worden teruggestuurd. Een groter origineel wordt
    /// afgekapt op 28 bytes (20-byte IP-header + 8), zoals klassiek gebruikelijk.
    pub fn build(self, original_datagram: &[u8]) -> Vec<u8> {
        let (typ, code) = self.type_code();
        let copy = original_datagram.len().min(28);
        let mut b = Vec::with_capacity(8 + copy);
        b.push(typ);
        b.push(code);
        b.extend_from_slice(&[0, 0]); // checksum-placeholder
        b.extend_from_slice(&[0, 0, 0, 0]); // ongebruikt (4 bytes)
        b.extend_from_slice(&original_datagram[..copy]);
        let cs = internet_checksum(&b);
        b[2..4].copy_from_slice(&cs.to_be_bytes());
        b
    }

    /// Parse een binnenkomend ICMP-foutbericht: geef de soort fout terug plus de
    /// teruggestuurde eerste bytes van het oorspronkelijke datagram (de IP-header +
    /// begin van de L4-header), zodat de afzender de fout aan zijn verbinding kan
    /// koppelen. Geeft `None` als het geen door ons herkende fout is of de checksum
    /// niet klopt. Het is de tegenhanger van [`build`](Self::build).
    pub fn parse(buf: &[u8]) -> Option<(IcmpError, Vec<u8>)> {
        if buf.len() < 8 || internet_checksum(buf) != 0 {
            return None;
        }
        let kind = match (buf[0], buf[1]) {
            (3, 1) => IcmpError::DestUnreachable(UnreachCode::Host),
            (3, 3) => IcmpError::DestUnreachable(UnreachCode::Port),
            (11, 0) => IcmpError::TimeExceeded,
            _ => return None,
        };
        // Bytes 4..8 zijn ongebruikt; daarna komt het oorspronkelijke datagram.
        Some((kind, buf[8..].to_vec()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echo_request_roundtrip() {
        let req = IcmpEcho {
            kind: IcmpType::EchoRequest,
            identifier: 0x1234,
            sequence: 7,
            payload: b"ping-data".to_vec(),
        };
        let bytes = req.build();
        let parsed = IcmpEcho::parse(&bytes).unwrap();
        assert_eq!(parsed, req);
    }

    #[test]
    fn reply_spiegelt_id_seq_payload() {
        let req = IcmpEcho {
            kind: IcmpType::EchoRequest,
            identifier: 42,
            sequence: 99,
            payload: b"abc".to_vec(),
        };
        let rep = IcmpEcho::reply_to(&req);
        assert_eq!(rep.kind, IcmpType::EchoReply);
        assert_eq!(rep.identifier, 42);
        assert_eq!(rep.sequence, 99);
        assert_eq!(rep.payload, b"abc");
        // Reply heeft geldige checksum.
        assert!(IcmpEcho::parse(&rep.build()).is_ok());
    }

    #[test]
    fn port_unreachable_shape_en_checksum() {
        // Doe alsof een UDP-datagram binnenkwam op een gesloten poort: 20-byte
        // IP-header + 8 bytes UDP-header.
        let mut orig = Vec::new();
        orig.extend_from_slice(&[0x45, 0, 0, 28]); // ver/ihl, tos, totlen
        orig.extend_from_slice(&[0; 16]); // rest van de IP-header
        orig.extend_from_slice(&[0xC0, 0, 0, 53, 0, 8, 0, 0]); // UDP src/dst/len/cs
        let msg = IcmpError::DestUnreachable(UnreachCode::Port).build(&orig);
        assert_eq!(msg[0], 3); // type = dest unreachable
        assert_eq!(msg[1], 3); // code = port
        assert_eq!(&msg[4..8], &[0, 0, 0, 0]); // ongebruikt veld
        // Hele bericht moet een geldige internet-checksum hebben.
        assert_eq!(internet_checksum(&msg), 0);
        // Het oorspronkelijke datagram zit erin terug (IP-header + 8 bytes).
        assert_eq!(&msg[8..], &orig[..]);
    }

    #[test]
    fn groot_datagram_wordt_afgekapt_op_28() {
        let orig = vec![0xABu8; 200];
        let msg = IcmpError::TimeExceeded.build(&orig);
        assert_eq!(msg[0], 11); // type = time exceeded
        assert_eq!(msg[1], 0);
        assert_eq!(msg.len(), 8 + 28); // header + afgekapt origineel
        assert_eq!(internet_checksum(&msg), 0);
    }

    #[test]
    fn build_parse_roundtrip_van_foutbericht() {
        let mut orig = Vec::new();
        orig.extend_from_slice(&[0x45, 0, 0, 28]);
        orig.extend_from_slice(&[0; 16]);
        orig.extend_from_slice(&[0xC0, 0, 0, 53, 0, 8, 0, 0]);
        let msg = IcmpError::DestUnreachable(UnreachCode::Port).build(&orig);
        let (kind, embedded) = IcmpError::parse(&msg).unwrap();
        assert_eq!(kind, IcmpError::DestUnreachable(UnreachCode::Port));
        assert_eq!(embedded, &orig[..]); // origineel datagram komt terug
    }

    #[test]
    fn parse_weigert_onbekend_type_en_corrupt() {
        // Een echo-reply (type 0) is geen fout → None.
        let echo = IcmpEcho { kind: IcmpType::EchoReply, identifier: 1, sequence: 1, payload: Vec::new() };
        assert!(IcmpError::parse(&echo.build()).is_none());
        // Eén byte flippen breekt de checksum → None.
        let mut msg = IcmpError::TimeExceeded.build(&[0u8; 28]);
        msg[9] ^= 0xFF;
        assert!(IcmpError::parse(&msg).is_none());
    }
}
