//! TPKT (RFC 1006) + X.224 class 0 (ISO 8073) as RDP uses them (MS-RDPBCGR 2.2.1).
//!
//! The first exchange of every RDP connection: a Connection Request carrying
//! the RDP Negotiation Request (we ask for TLS), answered by a Connection
//! Confirm with the Negotiation Response (or Failure). After that every PDU
//! rides in a Data TPDU (`02 F0 80`) inside a TPKT header.

use alloc::vec::Vec;

pub const PROTOCOL_RDP: u32 = 0;
pub const PROTOCOL_SSL: u32 = 1;
pub const PROTOCOL_HYBRID: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NegFailure {
    SslRequiredByServer,
    SslNotAllowedByServer,
    SslCertNotOnServer,
    InconsistentFlags,
    HybridRequiredByServer,
    Other(u32),
}

/// TPKT header: version 3, reserved, big-endian total length (header included).
pub fn tpkt(payload: &[u8]) -> Vec<u8> {
    let len = payload.len() + 4;
    let mut v = Vec::with_capacity(len);
    v.extend_from_slice(&[3, 0, (len >> 8) as u8, len as u8]);
    v.extend_from_slice(payload);
    v
}

/// The total length a TPKT header announces (None = not a TPKT header).
pub fn tpkt_len(hdr: &[u8]) -> Option<usize> {
    if hdr.len() < 4 || hdr[0] != 3 {
        return None;
    }
    Some(((hdr[2] as usize) << 8) | hdr[3] as usize)
}

/// X.224 Connection Request with an RDP Negotiation Request for `protocols`
/// (a bit set of PROTOCOL_*). `cookie` is the optional routing cookie line.
pub fn connection_request(protocols: u32, cookie: Option<&str>) -> Vec<u8> {
    let mut body = Vec::new();
    if let Some(c) = cookie {
        body.extend_from_slice(b"Cookie: mstshash=");
        body.extend_from_slice(c.as_bytes());
        body.extend_from_slice(b"\r\n");
    }
    // RDP_NEG_REQ: type 1, flags 0, length 8, requestedProtocols (LE u32)
    body.extend_from_slice(&[1, 0, 8, 0]);
    body.extend_from_slice(&protocols.to_le_bytes());
    let mut x = Vec::new();
    // LI = length of the rest of the TPDU header: 6 fixed + body
    x.push((6 + body.len()) as u8);
    x.push(0xE0); // CR TPDU, CDT 0
    x.extend_from_slice(&[0, 0]); // DST-REF
    x.extend_from_slice(&[0, 0]); // SRC-REF
    x.push(0); // class 0
    x.extend_from_slice(&body);
    tpkt(&x)
}

/// Parse a Connection Confirm (the payload after the TPKT header). Ok(selected
/// protocol) or Err(the server's failure code).
pub fn parse_connection_confirm(tpdu: &[u8]) -> Result<u32, NegFailure> {
    if tpdu.len() < 7 || tpdu[1] & 0xF0 != 0xD0 {
        return Err(NegFailure::Other(0));
    }
    // LI counts the bytes after itself: the 6 fixed header bytes (code, DST-REF,
    // SRC-REF, class) and then the RDP negotiation structure, if any.
    let li = tpdu[0] as usize;
    let end = (1 + li).min(tpdu.len());
    let body = if end > 7 { &tpdu[7..end] } else { &[][..] };
    if body.len() < 8 {
        return Ok(PROTOCOL_RDP); // no negotiation response: plain RDP security
    }
    let typ = body[0];
    let val = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
    match typ {
        2 => Ok(val), // RDP_NEG_RSP: selectedProtocol
        3 => Err(match val {
            1 => NegFailure::SslRequiredByServer,
            2 => NegFailure::SslNotAllowedByServer,
            3 => NegFailure::SslCertNotOnServer,
            4 => NegFailure::InconsistentFlags,
            5 => NegFailure::HybridRequiredByServer,
            o => NegFailure::Other(o),
        }),
        _ => Err(NegFailure::Other(typ as u32)),
    }
}

/// Wrap `payload` in a Data TPDU inside a TPKT.
pub fn data(payload: &[u8]) -> Vec<u8> {
    let mut x = Vec::with_capacity(payload.len() + 3);
    x.extend_from_slice(&[2, 0xF0, 0x80]);
    x.extend_from_slice(payload);
    tpkt(&x)
}

/// The payload of a Data TPDU (after its 3-byte header), or None.
pub fn data_payload(tpdu: &[u8]) -> Option<&[u8]> {
    if tpdu.len() < 3 || tpdu[0] != 2 || tpdu[1] & 0xF0 != 0xF0 {
        return None;
    }
    Some(&tpdu[3..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_request_shape() {
        let cr = connection_request(PROTOCOL_SSL, None);
        assert_eq!(&cr[..4], &[3, 0, 0, 19]); // 4 + 7 + 8
        assert_eq!(cr[4], 14); // LI
        assert_eq!(cr[5], 0xE0);
        assert_eq!(&cr[11..], &[1, 0, 8, 0, 1, 0, 0, 0]);
        assert_eq!(tpkt_len(&cr), Some(19));
    }

    #[test]
    fn connection_confirm_parses_response_and_failure() {
        let ok = [14u8, 0xD0, 0, 0, 0x12, 0x34, 0, 2, 0, 8, 0, 1, 0, 0, 0];
        assert_eq!(parse_connection_confirm(&ok), Ok(PROTOCOL_SSL));
        let fail = [14u8, 0xD0, 0, 0, 0, 0, 0, 3, 0, 8, 0, 5, 0, 0, 0];
        assert_eq!(parse_connection_confirm(&fail), Err(NegFailure::HybridRequiredByServer));
        let plain = [6u8, 0xD0, 0, 0, 0, 0, 0];
        assert_eq!(parse_connection_confirm(&plain), Ok(PROTOCOL_RDP));
    }

    #[test]
    fn data_tpdu_round_trip() {
        let d = data(&[9, 9, 9]);
        assert_eq!(&d[..4], &[3, 0, 0, 10]);
        assert_eq!(data_payload(&d[4..]), Some(&[9u8, 9, 9][..]));
    }
}
