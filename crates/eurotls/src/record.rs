//! TLS record layer (RFC 8446 §5.1): TLSPlaintext/TLSCiphertext framing. A
//! record is `type(1) || legacy_version(2)=0x0303 || length(2) || fragment`.
//! In TLS 1.3 encrypted records carry the outer type application_data(23); the
//! real content type is the last plaintext byte before the AEAD tag.

use alloc::vec::Vec;

pub const CT_CHANGE_CIPHER_SPEC: u8 = 20;
pub const CT_ALERT: u8 = 21;
pub const CT_HANDSHAKE: u8 = 22;
pub const CT_APPLICATION_DATA: u8 = 23;

pub struct Record {
    pub ctype: u8,
    pub fragment: Vec<u8>,
}

/// Maximum record fragment length (RFC 8446 §5.1): a TLSCiphertext may not be
/// larger than 2^14 + 256 bytes. A server claiming a larger length commits a
/// protocol violation (`record_overflow`) and is rejected.
pub const MAX_RECORD_LEN: usize = (1 << 14) + 256;

/// A record claimed a length > [`MAX_RECORD_LEN`] (RFC 8446 §5.1 `record_overflow`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordOverflow;

/// Try to read one complete record from `buf`.
/// - `Ok(Some((record, n)))`: a complete record consuming `n` bytes;
/// - `Ok(None)`: not enough bytes yet (wait for more);
/// - `Err(RecordOverflow)`: the claimed length exceeds `MAX_RECORD_LEN`
///   (malformed) — no reason to wait; the caller terminates the connection.
pub fn read_record(buf: &[u8]) -> Result<Option<(Record, usize)>, RecordOverflow> {
    if buf.len() < 5 {
        return Ok(None);
    }
    let ctype = buf[0];
    let len = u16::from_be_bytes([buf[3], buf[4]]) as usize;
    if len > MAX_RECORD_LEN {
        return Err(RecordOverflow);
    }
    if buf.len() < 5 + len {
        return Ok(None);
    }
    Ok(Some((Record { ctype, fragment: buf[5..5 + len].to_vec() }, 5 + len)))
}

/// Build a plain (unencrypted) record with the given content type.
pub fn build_record(ctype: u8, fragment: &[u8]) -> Vec<u8> {
    let mut r = Vec::with_capacity(5 + fragment.len());
    r.push(ctype);
    r.extend_from_slice(&[0x03, 0x03]);
    r.extend_from_slice(&(fragment.len() as u16).to_be_bytes());
    r.extend_from_slice(fragment);
    r
}

/// The 5-byte record header that serves as AAD for AEAD records (outer type
/// application_data, length = ciphertext incl. tag).
pub fn aead_aad(ciphertext_len: usize) -> [u8; 5] {
    [
        CT_APPLICATION_DATA,
        0x03,
        0x03,
        (ciphertext_len >> 8) as u8,
        ciphertext_len as u8,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_roundtrip_and_partial() {
        let r = build_record(CT_HANDSHAKE, b"hello");
        assert_eq!(r[0], CT_HANDSHAKE);
        assert_eq!(&r[1..3], &[0x03, 0x03]);
        let (rec, n) = read_record(&r).unwrap().unwrap();
        assert_eq!(rec.ctype, CT_HANDSHAKE);
        assert_eq!(rec.fragment, b"hello");
        assert_eq!(n, r.len());
        // Incomplete buffer -> Ok(None).
        assert!(matches!(read_record(&r[..4]), Ok(None)));
        assert!(matches!(read_record(&r[..r.len() - 1]), Ok(None)));
    }

    #[test]
    fn record_te_groot_wordt_geweigerd() {
        // Header claiming a length > MAX_RECORD_LEN → Err (protocol violation).
        let mut hdr = alloc::vec![CT_HANDSHAKE, 0x03, 0x03];
        hdr.extend_from_slice(&((MAX_RECORD_LEN as u16) + 1).to_be_bytes());
        assert!(read_record(&hdr).is_err());
        // Exactly MAX is allowed: with an incomplete buffer → Ok(None), not Err.
        let mut ok = alloc::vec![CT_HANDSHAKE, 0x03, 0x03];
        ok.extend_from_slice(&(MAX_RECORD_LEN as u16).to_be_bytes());
        assert!(matches!(read_record(&ok), Ok(None)));
    }
}
