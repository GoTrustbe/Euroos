//! Minimal, auditable **ZIP reader** for `.eupkg` packages (3E-6).
//!
//! `eupkg build` writes STORED (uncompressed) entries precisely so that ring 0
//! never needs an inflate implementation: this reader walks the End-Of-Central-
//! Directory → central directory → local headers, verifies the CRC-32 of every
//! extracted entry, and REFUSES any compression method other than STORED.
//! Package integrity/authenticity is additionally enforced above this layer
//! (Ed25519 over the manifest + SHA-256 pin of the binary).

use alloc::string::String;
use alloc::vec::Vec;

const EOCD_SIG: u32 = 0x0605_4b50; // "PK\x05\x06"
const CDIR_SIG: u32 = 0x0201_4b50; // "PK\x01\x02"
const LFH_SIG: u32 = 0x0403_4b50; // "PK\x03\x04"

fn rd_u16(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*b.get(o)?, *b.get(o + 1)?]))
}
fn rd_u32(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes([*b.get(o)?, *b.get(o + 1)?, *b.get(o + 2)?, *b.get(o + 3)?]))
}

/// CRC-32 (IEEE, reflected) — the ZIP entry checksum.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// One entry from the central directory.
pub struct ZipEntry {
    pub name: String,
    pub data: Vec<u8>,
}

/// Parse a STORED-only ZIP: every entry is extracted and CRC-verified.
/// `None` on: no/corrupt EOCD or central directory, a non-STORED entry,
/// out-of-bounds offsets, or a CRC mismatch (tampered entry).
pub fn parse(zip: &[u8]) -> Option<Vec<ZipEntry>> {
    // EOCD: scan backwards (the record is at the very end, modulo a comment).
    let mut eocd = None;
    let min = zip.len().saturating_sub(22 + 65_536);
    let mut i = zip.len().checked_sub(22)?;
    loop {
        if rd_u32(zip, i)? == EOCD_SIG {
            eocd = Some(i);
            break;
        }
        if i == min {
            break;
        }
        i -= 1;
    }
    let e = eocd?;
    let count = rd_u16(zip, e + 10)? as usize;
    let cd_off = rd_u32(zip, e + 16)? as usize;

    let mut out = Vec::with_capacity(count);
    let mut o = cd_off;
    for _ in 0..count {
        if rd_u32(zip, o)? != CDIR_SIG {
            return None;
        }
        let method = rd_u16(zip, o + 10)?;
        let crc = rd_u32(zip, o + 16)?;
        let csize = rd_u32(zip, o + 20)? as usize;
        let usize_ = rd_u32(zip, o + 24)? as usize;
        let nlen = rd_u16(zip, o + 28)? as usize;
        let elen = rd_u16(zip, o + 30)? as usize;
        let clen = rd_u16(zip, o + 32)? as usize;
        let lho = rd_u32(zip, o + 42)? as usize;
        let name = core::str::from_utf8(zip.get(o + 46..o + 46 + nlen)?).ok()?;

        // STORED only — refuse deflate & co. rather than carrying an inflater in ring 0.
        if method != 0 || csize != usize_ {
            return None;
        }
        // Local header: data starts after its (own) name + extra fields.
        if rd_u32(zip, lho)? != LFH_SIG {
            return None;
        }
        let lnlen = rd_u16(zip, lho + 26)? as usize;
        let lelen = rd_u16(zip, lho + 28)? as usize;
        let start = lho + 30 + lnlen + lelen;
        let data = zip.get(start..start + csize)?.to_vec();
        if crc32(&data) != crc {
            return None; // tampered/corrupt entry
        }
        out.push(ZipEntry { name: String::from(name), data });
        o += 46 + nlen + elen + clen;
    }
    Some(out)
}

/// Extract one named entry.
pub fn extract(zip: &[u8], name: &str) -> Option<Vec<u8>> {
    parse(zip)?.into_iter().find(|e| e.name == name).map(|e| e.data)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Hand-built STORED zip with one entry "a.txt" = b"hi".
    fn tiny_zip() -> Vec<u8> {
        let data = b"hi";
        let crc = crc32(data);
        let mut z = Vec::new();
        // LFH
        z.extend_from_slice(&LFH_SIG.to_le_bytes());
        z.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]); // ver,flags,method,time,date
        z.extend_from_slice(&crc.to_le_bytes());
        z.extend_from_slice(&(data.len() as u32).to_le_bytes());
        z.extend_from_slice(&(data.len() as u32).to_le_bytes());
        z.extend_from_slice(&[5, 0, 0, 0]); // nlen=5, elen=0
        z.extend_from_slice(b"a.txt");
        z.extend_from_slice(data);
        let cd = z.len();
        // Central directory
        z.extend_from_slice(&CDIR_SIG.to_le_bytes());
        z.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]); // vers,ver,flags,method,time,date
        z.extend_from_slice(&crc.to_le_bytes());
        z.extend_from_slice(&(data.len() as u32).to_le_bytes());
        z.extend_from_slice(&(data.len() as u32).to_le_bytes());
        z.extend_from_slice(&[5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]); // nlen..external attrs
        z.extend_from_slice(&0u32.to_le_bytes()); // LFH offset
        z.extend_from_slice(b"a.txt");
        let cd_len = z.len() - cd;
        // EOCD
        z.extend_from_slice(&EOCD_SIG.to_le_bytes());
        z.extend_from_slice(&[0, 0, 0, 0, 1, 0, 1, 0]);
        z.extend_from_slice(&(cd_len as u32).to_le_bytes());
        z.extend_from_slice(&(cd as u32).to_le_bytes());
        z.extend_from_slice(&[0, 0]);
        z
    }

    #[test]
    fn parses_stored_zip_and_verifies_crc() {
        let z = tiny_zip();
        assert_eq!(extract(&z, "a.txt").as_deref(), Some(b"hi".as_ref()));
    }

    #[test]
    fn corrupt_data_fails_crc() {
        let mut z = tiny_zip();
        // Flip a bit in the entry data ("hi" sits right before the central dir).
        let pos = z.windows(2).position(|w| w == b"hi").unwrap();
        z[pos] ^= 0x01;
        assert!(parse(&z).is_none());
    }

    #[test]
    fn refuses_non_stored_method() {
        let mut z = tiny_zip();
        // Set method=8 (deflate) in the central directory entry.
        let cd = z.windows(4).position(|w| w == CDIR_SIG.to_le_bytes()).unwrap();
        z[cd + 10] = 8;
        assert!(parse(&z).is_none());
    }
}
