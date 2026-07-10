//! **EuroFlate** — from-scratch `no_std` DEFLATE/INFLATE (RFC 1951) plus the
//! zlib (RFC 1950) and gzip (RFC 1952) wrappers.
//!
//! This is the keystone for real Office documents: `.docx`/`.xlsx`/`.pptx` are
//! DEFLATE-compressed ZIP containers, so reading them needs a full INFLATE that
//! handles **dynamic Huffman** (what real writers emit). [`inflate`] does; its
//! KAT test decodes real `zlib`-level-9 streams byte-for-byte. [`deflate`] emits
//! valid fixed-Huffman streams that real tools read (proven on the host by
//! round-tripping through real `zlib`).
//!
//! Adler-32 (zlib) and CRC-32 (gzip) checksums are verified/produced so the
//! wrappers interoperate with real toolchains, not just with ourselves.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

mod deflate;
mod inflate;

use alloc::vec::Vec;

pub use deflate::{deflate, deflate_then_inflate};
pub use inflate::{inflate, InflateError};

/// Adler-32 (RFC 1950) over `data`.
pub fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65521;
    let mut a = 1u32;
    let mut b = 0u32;
    for &byte in data {
        a = (a + byte as u32) % MOD;
        b = (b + a) % MOD;
    }
    (b << 16) | a
}

/// CRC-32 (IEEE, gzip) over `data`.
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

/// Wrap a raw DEFLATE stream in a zlib (RFC 1950) container: 2-byte header +
/// deflate + big-endian Adler-32 of the *uncompressed* data.
pub fn zlib_compress(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(0x78); // CMF: deflate, 32K window
    out.push(0x9C); // FLG: default level, check bits make (0x78<<8|0x9C)%31==0
    out.extend_from_slice(&deflate(data));
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

/// Decompress a zlib (RFC 1950) stream: skip the 2-byte header, inflate, and
/// verify the trailing Adler-32.
pub fn zlib_decompress(data: &[u8]) -> Result<Vec<u8>, InflateError> {
    if data.len() < 6 {
        return Err(InflateError::UnexpectedEof);
    }
    // CMF/FLG checksum + method sanity.
    if data[0] & 0x0F != 8 {
        return Err(InflateError::BadBlockType);
    }
    let body = &data[2..data.len() - 4];
    let out = inflate(body)?;
    let want = u32::from_be_bytes([
        data[data.len() - 4],
        data[data.len() - 3],
        data[data.len() - 2],
        data[data.len() - 1],
    ]);
    if adler32(&out) != want {
        return Err(InflateError::BadCode); // checksum mismatch
    }
    Ok(out)
}

/// Decompress a gzip (RFC 1952) stream (skips the header + optional fields,
/// inflates, verifies the trailing CRC-32 and ISIZE).
pub fn gzip_decompress(data: &[u8]) -> Result<Vec<u8>, InflateError> {
    if data.len() < 18 || data[0] != 0x1F || data[1] != 0x8B || data[2] != 8 {
        return Err(InflateError::BadBlockType);
    }
    let flg = data[3];
    let mut pos = 10;
    if flg & 0x04 != 0 {
        // FEXTRA
        if pos + 2 > data.len() {
            return Err(InflateError::UnexpectedEof);
        }
        let xlen = u16::from_le_bytes([data[pos], data[pos + 1]]) as usize;
        pos += 2 + xlen;
    }
    if flg & 0x08 != 0 {
        // FNAME (zero-terminated)
        while pos < data.len() && data[pos] != 0 {
            pos += 1;
        }
        pos += 1;
    }
    if flg & 0x10 != 0 {
        // FCOMMENT
        while pos < data.len() && data[pos] != 0 {
            pos += 1;
        }
        pos += 1;
    }
    if flg & 0x02 != 0 {
        pos += 2; // FHCRC
    }
    if pos + 8 > data.len() {
        return Err(InflateError::UnexpectedEof);
    }
    let body = &data[pos..data.len() - 8];
    let out = inflate(body)?;
    let want_crc = u32::from_le_bytes([
        data[data.len() - 8],
        data[data.len() - 7],
        data[data.len() - 6],
        data[data.len() - 5],
    ]);
    if crc32(&out) != want_crc {
        return Err(InflateError::BadCode);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adler32_known_vector() {
        // zlib's canonical Adler-32("Wikipedia") = 0x11E60398.
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }

    #[test]
    fn crc32_known_vector() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn deflate_inflate_roundtrip() {
        let cases: &[&[u8]] = &[
            b"",
            b"a",
            b"Hello, EuroOS!",
            b"ABABABABABABABABABABABAB",
            &[0u8; 300],
            b"The quick brown fox jumps over the lazy dog. The quick brown fox again.",
        ];
        for c in cases {
            let round = deflate_then_inflate(c).expect("roundtrip");
            assert_eq!(&round, c, "roundtrip failed for {:?} bytes", c.len());
        }
    }

    #[test]
    fn zlib_wrapper_roundtrip_with_adler() {
        let data = b"EuroOS zlib container test payload, repeated. ".repeat(5);
        let comp = zlib_compress(&data);
        assert_eq!(comp[0], 0x78);
        assert_eq!(((comp[0] as u16) << 8 | comp[1] as u16) % 31, 0);
        assert_eq!(zlib_decompress(&comp).unwrap(), data);
    }

    #[test]
    fn zlib_detects_corrupt_checksum() {
        let mut comp = zlib_compress(b"payload");
        let n = comp.len();
        comp[n - 1] ^= 0xFF; // trash the Adler-32
        assert!(zlib_decompress(&comp).is_err());
    }
}
