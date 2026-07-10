//! A real **ZIP container** with DEFLATE — the layer that was missing so that
//! `.docx`/`.xlsx`/`.pptx` (which are DEFLATE-compressed ZIPs written by real
//! tools) can be opened and saved end-to-end. Reads STORED **and** DEFLATE
//! entries (via [`euroflate`]) with CRC-32 verification; writes DEFLATE entries
//! that real tools (LibreOffice/`unzip`) accept.
//!
//! Deliberately small: no ZIP64, no encryption, no data descriptors on read
//! (Office files use a normal central directory) — enough for Office packages.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

const LFH_SIG: u32 = 0x0403_4b50;
const CDIR_SIG: u32 = 0x0201_4b50;
const EOCD_SIG: u32 = 0x0605_4b50;

fn rd_u16(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*b.get(o)?, *b.get(o + 1)?]))
}
fn rd_u32(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes([*b.get(o)?, *b.get(o + 1)?, *b.get(o + 2)?, *b.get(o + 3)?]))
}

#[derive(Debug, Clone)]
pub struct ZipEntry {
    pub name: String,
    pub data: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ZipError {
    Truncated,
    BadSignature,
    UnsupportedMethod(u16),
    Crc,
    Inflate,
}

/// Read all entries from a ZIP archive (STORED + DEFLATE), CRC-verifying each.
pub fn read(zip: &[u8]) -> Result<Vec<ZipEntry>, ZipError> {
    // Find the End-Of-Central-Directory record (scan back over any comment).
    let mut e = None;
    if zip.len() >= 22 {
        let min = zip.len().saturating_sub(22 + 65_536);
        let mut i = zip.len() - 22;
        loop {
            if rd_u32(zip, i) == Some(EOCD_SIG) {
                e = Some(i);
                break;
            }
            if i == min {
                break;
            }
            i -= 1;
        }
    }
    let e = e.ok_or(ZipError::Truncated)?;
    let count = rd_u16(zip, e + 10).ok_or(ZipError::Truncated)? as usize;
    let cd_off = rd_u32(zip, e + 16).ok_or(ZipError::Truncated)? as usize;

    let mut out = Vec::with_capacity(count);
    let mut o = cd_off;
    for _ in 0..count {
        if rd_u32(zip, o) != Some(CDIR_SIG) {
            return Err(ZipError::BadSignature);
        }
        let method = rd_u16(zip, o + 10).ok_or(ZipError::Truncated)?;
        let crc = rd_u32(zip, o + 16).ok_or(ZipError::Truncated)?;
        let csize = rd_u32(zip, o + 20).ok_or(ZipError::Truncated)? as usize;
        let usize_ = rd_u32(zip, o + 24).ok_or(ZipError::Truncated)? as usize;
        let nlen = rd_u16(zip, o + 28).ok_or(ZipError::Truncated)? as usize;
        let elen = rd_u16(zip, o + 30).ok_or(ZipError::Truncated)? as usize;
        let clen = rd_u16(zip, o + 32).ok_or(ZipError::Truncated)? as usize;
        let lho = rd_u32(zip, o + 42).ok_or(ZipError::Truncated)? as usize;
        let name = core::str::from_utf8(zip.get(o + 46..o + 46 + nlen).ok_or(ZipError::Truncated)?)
            .map_err(|_| ZipError::Truncated)?
            .to_string();

        // Locate the entry data via the local header (its own name/extra lengths).
        if rd_u32(zip, lho) != Some(LFH_SIG) {
            return Err(ZipError::BadSignature);
        }
        let lnlen = rd_u16(zip, lho + 26).ok_or(ZipError::Truncated)? as usize;
        let lelen = rd_u16(zip, lho + 28).ok_or(ZipError::Truncated)? as usize;
        let start = lho + 30 + lnlen + lelen;
        let comp = zip.get(start..start + csize).ok_or(ZipError::Truncated)?;

        let data = match method {
            0 => comp.to_vec(), // STORED
            8 => euroflate::inflate(comp).map_err(|_| ZipError::Inflate)?, // DEFLATE
            m => return Err(ZipError::UnsupportedMethod(m)),
        };
        if data.len() != usize_ || euroflate::crc32(&data) != crc {
            return Err(ZipError::Crc);
        }
        out.push(ZipEntry { name, data });
        o += 46 + nlen + elen + clen;
    }
    Ok(out)
}

/// Extract one named entry's bytes.
pub fn read_entry(zip: &[u8], name: &str) -> Result<Vec<u8>, ZipError> {
    read(zip)?.into_iter().find(|z| z.name == name).map(|z| z.data).ok_or(ZipError::Truncated)
}

/// Write a ZIP archive, DEFLATE-compressing every entry. Real tools read it.
pub fn write(entries: &[ZipEntry]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();

    for entry in entries {
        let name = entry.name.as_bytes();
        let crc = euroflate::crc32(&entry.data);
        let compressed = euroflate::deflate(&entry.data);
        // If compression didn't help (tiny/incompressible), STORE instead so we
        // never grow the payload — matches what real zippers do.
        let (method, body): (u16, &[u8]) =
            if compressed.len() < entry.data.len() { (8, &compressed) } else { (0, &entry.data) };
        let lho = out.len() as u32;

        // Local file header.
        out.extend_from_slice(&LFH_SIG.to_le_bytes());
        out.extend_from_slice(&[20, 0]); // version needed
        out.extend_from_slice(&[0, 0]); // flags
        out.extend_from_slice(&method.to_le_bytes());
        out.extend_from_slice(&[0, 0, 0, 0]); // time+date
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&(entry.data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&[0, 0]); // extra len
        out.extend_from_slice(name);
        out.extend_from_slice(body);

        // Central directory record.
        central.extend_from_slice(&CDIR_SIG.to_le_bytes());
        central.extend_from_slice(&[20, 0, 20, 0]); // version made/needed
        central.extend_from_slice(&[0, 0]); // flags
        central.extend_from_slice(&method.to_le_bytes());
        central.extend_from_slice(&[0, 0, 0, 0]); // time+date
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&(body.len() as u32).to_le_bytes());
        central.extend_from_slice(&(entry.data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(name.len() as u16).to_le_bytes());
        central.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]); // extra/comment/disk/attrs
        central.extend_from_slice(&lho.to_le_bytes());
        central.extend_from_slice(name);
    }

    let cd_off = out.len() as u32;
    let cd_len = central.len() as u32;
    let n = entries.len() as u16;
    out.extend_from_slice(&central);
    out.extend_from_slice(&EOCD_SIG.to_le_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]); // disk numbers
    out.extend_from_slice(&n.to_le_bytes());
    out.extend_from_slice(&n.to_le_bytes());
    out.extend_from_slice(&cd_len.to_le_bytes());
    out.extend_from_slice(&cd_off.to_le_bytes());
    out.extend_from_slice(&[0, 0]); // comment len
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_then_read_roundtrip_with_deflate() {
        let entries = vec![
            ZipEntry { name: "hello.txt".to_string(), data: b"Hello EuroOS ".repeat(20) },
            ZipEntry { name: "small".to_string(), data: b"x".to_vec() },
        ];
        let zip = write(&entries);
        let back = read(&zip).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].name, "hello.txt");
        assert_eq!(back[0].data, b"Hello EuroOS ".repeat(20));
        assert_eq!(read_entry(&zip, "small").unwrap(), b"x");
    }

    #[test]
    fn corrupt_entry_fails_crc() {
        let entries = vec![ZipEntry { name: "a".to_string(), data: b"data payload here".repeat(5) }];
        let mut zip = write(&entries);
        // Flip a byte inside the first entry's compressed body.
        zip[40] ^= 0xFF;
        assert!(read(&zip).is_err());
    }
}
